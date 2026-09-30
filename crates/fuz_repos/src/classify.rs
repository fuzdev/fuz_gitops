//! Probe facts → each branch's relation and verdict, the entry's
//! `needs_human` reasons, and whether its checkout is at rest; a missing
//! entry → its clone verdict. Pure.
//!
//! The verdict is the one place sync's per-branch decision is made: `status`
//! previews it, `sync` executes it, and JSON consumers read it rather than
//! re-deriving policy from relations.

use std::path::Path;

use serde::Serialize;

use crate::busy::{Detection, EntrySessions};
use crate::gitdir::is_valid_refname;
use crate::porcelain::{BranchConfig, ConfigFacts, OriginKeys, OriginUrl, RefFacts, Track};
use crate::probe::{BranchFacts, RepoFacts};
use crate::registry::{Entry, RepoUrl};
use crate::report::{UnregisteredClone, UnregisteredKind};
use crate::sessions::Session;
use crate::state::{
    AtRest, BranchNeedsHuman, BranchStatus, CleanupReason, CloneRecipe, CloneVerdict, Head, HeldBy,
    InProgressOp, Prune, PruneLoss, RefreshVerdict, Relation, SyncAction, Uncommitted,
    UnprobedHead, UnprobedWhy, UnprobedWorktree, UnprobedWorktreeStatus, Verdict,
};
use crate::url::{RemoteParts, remote_parts, without_userinfo};

/// Why `sync` would stop on an entry and leave it to a person. Branch-level
/// reasons are on each branch's `Verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsHuman {
    /// The dir exists but git finds no repo there; `detail` says why (an
    /// empty dir, or git's message).
    NotARepo {
        detail: String,
    },
    OperationInProgress {
        checkout: String,
        op: InProgressOp,
    },
    /// `origin` isn't the registry's `url`, or has none. `expected` is the
    /// URL it should have (SSH when owned, else HTTPS); `fix`, how to set it
    /// with a command that can. A third-party reference's refresh is held
    /// for it, never fetched (`refresh_verdict`).
    OriginMismatch {
        origin: OriginRemote,
        expected: String,
        fix: OriginFix,
    },
    /// A refresh the run asks of a third-party reference whose `origin` is
    /// the registry's repo, but whose fetch wouldn't reach it over HTTPS,
    /// the only transport a reference is fetched over: `fetch_url` is where
    /// it would reach, as git resolves it (`insteadOf` applied; a
    /// credential in its userinfo redacted as `***`) — SSH, `http://`,
    /// `git://`, or another repo a rewrite names. `expected` is the
    /// registry's HTTPS URL. The refresh is held (`HeldBy::OriginNotHttps`),
    /// never fetched; the rest of the entry goes on. `fix` is how to point
    /// `origin` at `expected`, as `origin_mismatch`'s says — `None` when a
    /// `url.<base>.insteadOf` rewrite changes origin's URL, which setting
    /// the URL may not undo: the rewrite is what to change.
    OriginNotHttps {
        fetch_url: String,
        expected: String,
        fix: Option<OriginFix>,
    },
    /// An owned entry whose `origin` is the registry's repo, but whose
    /// fetch from it wouldn't reach that repo as sync fetches it: `fetch_url`
    /// is where it would reach, as git resolves it (`insteadOf` applied; a
    /// credential in its userinfo redacted as `***`) — another repo a
    /// rewrite names, or, in a partial clone, whose checkouts fetch missing
    /// objects from origin on demand, a transport that fetch may not take
    /// (neither SSH nor HTTPS: `fetch_url_mismatch`). `expected` is the
    /// registry's SSH URL. The entry is held whole and never fetched: the
    /// fetch would fill `refs/remotes/origin/*` with another repo's history.
    /// `fix` is how to point `origin` at `expected`, as `origin_mismatch`'s
    /// says — `None` when a `url.<base>.insteadOf` rewrite changes origin's
    /// URL, which setting the URL may not undo: the rewrite is what to
    /// change.
    FetchUrlMismatch {
        fetch_url: String,
        expected: String,
        fix: Option<OriginFix>,
    },
    /// A worktree's git dir (or `<commondir>/worktrees/` itself) can't be
    /// read, so whether an operation is in progress there is unknowable.
    WorktreeUnreadable {
        path: String,
    },
    DefaultBranchMissing {
        branch: String,
    },
    DefaultBranchNoUpstream {
        branch: String,
    },
    /// The branch the entry follows tracks an origin branch that's gone
    /// from origin — the remote's default branch renamed (`master` to
    /// `main`) or deleted. Never cleanup: it's the branch the entry lives
    /// on, and deleting it, or the worktree it's in, isn't the fix. The
    /// branch itself reads `LocalOnly` or `Quiet`, as one with no upstream
    /// does.
    DefaultBranchGone {
        branch: String,
    },
    UnexpectedDetached {
        checkout: String,
    },
    /// A checkout's path can't be resolved — `path` is as far as it got, in
    /// a dir the tool can't search or a symlink loop — so whether a live
    /// session works in it can't be told. It may be busy: like a busy
    /// checkout, it holds the branches checked out there (`BusyUnknown`),
    /// and the entry's other branches act.
    CheckoutUnresolvable {
        checkout: String,
        path: String,
        error: String,
    },
    /// A git dir no worktree list names shares the repo's refs — made by
    /// hand, with a `commondir` file naming its common dir, or by
    /// `git-new-workdir`, with a symlinked `refs` — and a live session works
    /// through it, so a commit there moves the branch its HEAD names. It's
    /// held as though busy (`BusyUnknown`), every branch when its HEAD is
    /// unknown, and the entry's other branches act. Seen only with a session
    /// in it: git itself doesn't know it's there.
    UnlistedGitDir {
        git_dir: String,
        head: UnprobedHead,
        busy: Vec<Session>,
    },
    /// A push through `origin` would go somewhere other than the registry's
    /// repo over SSH: `push_urls` is where, as git resolves it (`pushurl`
    /// over `url`, `insteadOf` and `pushInsteadOf` applied; a credential in
    /// a URL's userinfo redacted as `***`) — another repo, another
    /// transport, or several URLs, each of which a push would reach.
    /// `expected` is the registry's SSH URL. Every push is held
    /// (`PushUrl`); fetches and fast-forwards go on.
    PushUrlMismatch {
        push_urls: Vec<String>,
        expected: String,
    },
    /// A missing entry whose `url` names the same repo as another entry's,
    /// `with` (`Entry::same_repo_as`): its dir may have been a linked
    /// worktree of that repo (its record since pruned), and a clone would
    /// make a second, independent copy — the tool never guesses which is
    /// meant. Its clone is held (`HeldBy::Entry`); a person clones it, or
    /// adds the worktree, by hand.
    CloneSharesRepo {
        with: String,
    },
    /// A missing entry whose repo is already cloned at `dir`, a dir at the
    /// workspace root no registry entry claims, whose origin names the
    /// entry's repo, as the unregistered scan read it — or a rename of it,
    /// its name differing only in ASCII case and `-` against `_`
    /// (`names_repo_loosely`): likely the entry's own checkout under
    /// another name, and a clone would make a second, independent copy. Its
    /// clone is held (`HeldBy::Entry`); a person renames the dir to the
    /// entry's, or points the entry's `dir` at it. Found only when the scan
    /// ran — every run with a missing entry in it — and only through an
    /// origin the scan could read: a clone whose origin names the repo by
    /// an unrelated old name isn't caught.
    ClonedUnregistered {
        dir: String,
    },
}

impl NeedsHuman {
    /// Whether the reason stops sync on the whole entry, holding every
    /// branch's action: an operation mid-way owns the checkout (and one that
    /// can't be ruled out counts the same), and a wrong origin, or a fetch
    /// from it that reaches elsewhere, would move branches to another
    /// repo's history. The rest concern one branch or
    /// one checkout's HEAD (an unresolvable checkout holds the branches
    /// checked out there, as a busy one does), and leave the other branches
    /// safe to sync.
    pub const fn holds_entry(&self) -> bool {
        match self {
            Self::NotARepo { .. }
            | Self::OperationInProgress { .. }
            | Self::OriginMismatch { .. }
            | Self::FetchUrlMismatch { .. }
            | Self::WorktreeUnreadable { .. }
            | Self::CloneSharesRepo { .. }
            | Self::ClonedUnregistered { .. } => true,
            Self::DefaultBranchMissing { .. }
            | Self::DefaultBranchNoUpstream { .. }
            | Self::DefaultBranchGone { .. }
            | Self::UnexpectedDetached { .. }
            | Self::CheckoutUnresolvable { .. }
            | Self::UnlistedGitDir { .. }
            | Self::PushUrlMismatch { .. }
            | Self::OriginNotHttps { .. } => false,
        }
    }
}

/// The `origin` remote as git sees it (every config scope), when it isn't
/// the registry's `url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginRemote {
    /// Another URL: the one git fetches from, the first of the list (an
    /// empty value resets the list). A credential in its userinfo is
    /// redacted as `***`.
    Url { url: String },
    /// Known to git — some `remote.origin.*` key is set — but with no URL,
    /// or a list an empty value reset.
    NoUrl,
    /// No `remote.origin.*` key in any scope.
    Missing,
}

/// How to point `origin` at the registry's URL.
///
/// Decided from what each command can edit: `git remote` writes the repo's
/// own config file, and refuses or accepts by whether the repo's scope
/// configures `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OriginFix {
    /// `git remote add origin <expected>`: no `remote.origin.*` key in the
    /// repo's scope (`remote set-url` would say `No such remote`).
    Add,
    /// `git remote set-url origin <expected>`: `origin` is the repo's, with
    /// at most one URL, in its own config file.
    SetUrl,
    /// Fix `remote.origin.url` by hand, for `reason`.
    ByHand { reason: OriginByHand },
}

/// Why no `git remote` command fits an origin's fix. When several apply,
/// the first listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginByHand {
    /// A URL git reads from beyond the repo's own file (global or system
    /// config, an included file, the worktree config), which no `git remote`
    /// command edits and which would still come first.
    OutsideRepoFile,
    /// A valueless `url` (no `=`), which breaks every git remote command
    /// (`missing value for 'remote.origin.url'`).
    ValuelessUrl,
    /// An empty value among several: it resets the list, and with several
    /// values `set-url` fails (`remote.origin.url has multiple values`). A
    /// single empty value is `SetUrl`'s to replace.
    EmptyValue,
    /// Several URLs, where a plain `set-url` fails (`remote.origin.url has
    /// multiple values`) and a value-pattern one fails too whenever the first
    /// is rewritten by `insteadOf`, duplicated, or spelled with other
    /// userinfo.
    SeveralUrls,
}

impl OriginFix {
    /// The fix for a repo whose `origin` needs the registry's URL.
    pub fn decide(config: &ConfigFacts) -> Self {
        let urls = &config.origin_urls;
        let by_hand = |reason| Self::ByHand { reason };
        if urls.iter().any(|v| !v.in_repo_file) {
            return by_hand(OriginByHand::OutsideRepoFile);
        }
        if urls.iter().any(|v| v.value.is_none()) {
            return by_hand(OriginByHand::ValuelessUrl);
        }
        if config.origin_keys != OriginKeys::InRepo {
            return Self::Add;
        }
        if urls.len() > 1 {
            return by_hand(if urls.iter().any(OriginUrl::resets) {
                OriginByHand::EmptyValue
            } else {
                OriginByHand::SeveralUrls
            });
        }
        // at most one value, maybe empty: `set-url` replaces it
        Self::SetUrl
    }
}

/// What `classify` derives for a present repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub branches: Vec<BranchStatus>,
    pub needs_human: Vec<NeedsHuman>,
    /// The repo's unprobed worktrees, each with what removing it would do.
    pub unprobed: Vec<UnprobedWorktreeStatus>,
    /// Whether the primary checkout is at rest where the registry puts it.
    pub at_rest: AtRest,
}

/// What dropping an unprobed worktree's git dir would do — that worktree's
/// alone, as `git worktree remove <path>` drops it.
///
/// `None` unless it's `Prunable` (its dir gone); `Safe` when it loses
/// nothing; else what it loses — an operation's state, a HEAD that may be
/// the only ref to its commit (detached, unreadable, or on a branch that no
/// longer exists), what its git dir alone holds (submodules' repos,
/// per-worktree refs, staged changes — an index that can't be compared
/// counts, unless its HEAD is already lost; a git dir that can't be matched
/// counts as unknown), or, when a worktree git dir of
/// the repo names its worktree relatively, whatever git misreads (git
/// versions resolve a relative `gitdir` differently, so no path of the repo
/// is certain).
fn prune(u: &UnprobedWorktree, facts: &RepoFacts) -> Option<Prune> {
    if u.why != UnprobedWhy::Prunable {
        return None;
    }
    let mut losses = Vec::new();
    if let Some(op) = u.in_progress {
        losses.push(PruneLoss::Operation { op });
    }
    match &u.head {
        UnprobedHead::Branch { name } => {
            if !facts.branches.iter().any(|b| b.branch.name == *name) {
                losses.push(PruneLoss::MissingBranch { name: name.clone() });
            }
        }
        UnprobedHead::Detached { .. } => losses.push(PruneLoss::DetachedHead),
        UnprobedHead::Unknown => losses.push(PruneLoss::UnknownHead),
    }
    if u.git_dir.is_none() {
        losses.push(PruneLoss::UnmatchedGitDir);
    }
    if let Some(holds) = &u.holds {
        if holds.submodules {
            losses.push(PruneLoss::Submodules);
        }
        if holds.worktree_refs {
            losses.push(PruneLoss::WorktreeRefs);
        }
        // not told: a lost HEAD already says the index can't be judged
        let head_lost = losses
            .iter()
            .any(|l| matches!(l, PruneLoss::UnknownHead | PruneLoss::MissingBranch { .. }));
        if holds.staged.unwrap_or(!head_lost) {
            losses.push(PruneLoss::StagedChanges);
        }
    }
    if let Some(git_dir) = &facts.relative_gitdir {
        losses.push(PruneLoss::RelativeGitdir {
            git_dir: git_dir.to_string_lossy().into_owned(),
        });
    }
    Some(if losses.is_empty() {
        Prune::Safe
    } else {
        Prune::Loses { losses }
    })
}

/// Which references a run asks to refresh — like a locked dependency, a
/// reference is otherwise left as it is: a third-party one never fetched,
/// a pin never touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Refresh {
    /// None: no targets, no `--references`.
    #[default]
    Unasked,
    /// The run's targets name every entry in it: each third-party
    /// reference among them is refreshed (unless its origin drifted or
    /// isn't reached over HTTPS: `refresh_verdict`), and each pin refused.
    Named,
    /// `--references`, with no targets: every third-party reference is
    /// refreshed; pins, named by no one, stay quiet.
    References,
}

/// What `refresh` asks of `entry` from the registry alone, before its
/// origin is read.
///
/// `None` for an owned entry not pinned (always synced, never
/// "refreshed"), and for any entry the run doesn't ask about. The verdict
/// is `refresh_verdict`'s, once the repo's config is read; for a repo whose
/// config couldn't be, never fetched, only a pin's refusal stands. The
/// probe reads where a refresh's fetch would reach when this acts.
pub const fn refresh_intent(entry: &Entry, refresh: Refresh) -> Option<RefreshVerdict> {
    match refresh {
        Refresh::Named if entry.pinned => Some(RefreshVerdict::Held { by: HeldBy::Pinned }),
        Refresh::Named | Refresh::References if !entry.writable && !entry.pinned => {
            Some(RefreshVerdict::Act)
        }
        Refresh::Unasked | Refresh::Named | Refresh::References => None,
    }
}

/// What `refresh` asks of `entry` (`RefreshVerdict`), its repo's `config`
/// read.
///
/// `refresh_intent`, but a refresh of a repo whose origin isn't the
/// registry's repo (`origin_drift`: another URL, or none) is held
/// (`HeldBy::Entry`, the entry's `origin_mismatch` reason) — never fetched,
/// since the fetch would bring in another repo's history, over whatever
/// transport that URL names. So is one whose fetch wouldn't reach the
/// registry's repo over HTTPS (`fetches_over_https`: an SSH-form origin, or
/// an `insteadOf` rewrite), held by `HeldBy::OriginNotHttps` (the entry's
/// `origin_not_https` reason): a reference is fetched over HTTPS alone,
/// and that fetch would fail. The probe decides its fetch by this verdict.
pub fn refresh_verdict(
    entry: &Entry,
    refresh: Refresh,
    config: &ConfigFacts,
) -> Option<RefreshVerdict> {
    match refresh_intent(entry, refresh) {
        Some(RefreshVerdict::Act) if origin_drift(entry, config).is_some() => {
            Some(RefreshVerdict::Held { by: HeldBy::Entry })
        }
        Some(RefreshVerdict::Act) if !fetches_over_https(entry, config) => {
            Some(RefreshVerdict::Held {
                by: HeldBy::OriginNotHttps,
            })
        }
        verdict => verdict,
    }
}

/// Whether a fetch from `origin` reaches the registry's repo over HTTPS:
/// the URL git resolves (`ConfigFacts::origin_fetch_url`, rewrites
/// applied) is `https://`, naming the repo as `origin_matches` reads it.
/// False when the probe didn't read it.
fn fetches_over_https(entry: &Entry, config: &ConfigFacts) -> bool {
    config
        .origin_fetch_url
        .as_deref()
        .is_some_and(|u| u.starts_with("https://") && origin_matches(u, &entry.url))
}

/// Where an owned entry's fetch from `origin` would reach, when that isn't
/// the registry's repo as sync fetches it.
///
/// That is, the URL git resolves (`ConfigFacts::origin_fetch_url`,
/// rewrites applied) names another repo (`origin_matches`), or, in a
/// partial clone, a transport its lazy fetch may not take
/// (`lazy_transport`: neither SSH nor HTTPS). `None` when it is, for an
/// entry not owned or pinned (a reference's refresh has its own check,
/// `fetches_over_https`; a pin is never fetched), and when the probe didn't
/// read it.
pub fn fetch_url_mismatch<'a>(entry: &Entry, config: &'a ConfigFacts) -> Option<&'a str> {
    if !entry.writable || entry.pinned {
        return None;
    }
    let url = config.origin_fetch_url.as_deref()?;
    let reaches = origin_matches(url, &entry.url)
        && (config.partial_filter.is_none() || lazy_transport(url).is_some());
    (!reaches).then_some(url)
}

/// The one transport a lazy fetch from `origin` may take: its own.
///
/// `ssh` for an SSH URL (`ssh://`, its `git+ssh` spellings, or scp-like),
/// `https` for an HTTPS one — whoever owns the repo, so an owned partial
/// clone whose origin is HTTPS fills its checkout over HTTPS. `None` for
/// anything else (plain `http`, `git://`, a local path, a URL
/// `remote_parts` rejects): no lazy fetch.
pub fn lazy_transport(origin: &str) -> Option<&'static str> {
    let parts = remote_parts(origin)?;
    if parts.ssh {
        Some("ssh")
    } else if origin.starts_with("https://") {
        Some("https")
    } else {
        None
    }
}

/// Whether an entry's branches are compared against origin: owned, or a
/// third-party reference the run refreshes (`refresh_verdict`).
fn tracked(entry: &Entry, refresh: Refresh, config: &ConfigFacts) -> bool {
    entry.writable
        || matches!(
            refresh_verdict(entry, refresh, config),
            Some(RefreshVerdict::Act)
        )
}

/// Classifies a present repo's facts against its registry entry.
///
/// With the live sessions in its checkouts and which references the run
/// refreshes. Owned entries get a relation per branch, and so does a
/// third-party reference the run refreshes (`refresh_verdict`), whose
/// branch ahead is local-only work: it's never pushed. Any other
/// third-party reference is never compared against a remote: it keeps only
/// branches with commits on no remote, as `Untracked` — local work that can
/// never be pushed.
pub fn classify(
    entry: &Entry,
    facts: &RepoFacts,
    sessions: &EntrySessions,
    refresh: Refresh,
) -> Classified {
    let needs_human = needs_human(entry, facts, sessions, refresh);
    let entry_held = needs_human.iter().any(NeedsHuman::holds_entry);
    let push_url = needs_human
        .iter()
        .any(|r| matches!(r, NeedsHuman::PushUrlMismatch { .. }));
    let tracked = tracked(entry, refresh, &facts.config);
    // `<commondir>/worktrees/` itself can't be read: which branches its
    // worktrees are on is unknown, so no branch is cleanup — as with any
    // unprobed worktree whose HEAD is unknown (`CheckoutsOn::head_unknown`)
    let worktrees_dir = facts.common_dir.join("worktrees");
    let worktrees_unread = needs_human.iter().any(
        |r| matches!(r, NeedsHuman::WorktreeUnreadable { path } if Path::new(path) == worktrees_dir),
    );
    let branches: Vec<BranchStatus> = facts
        .branches
        .iter()
        .filter_map(|b| {
            let relation = if tracked {
                relation(b, facts)
            } else if b.unique_commits > 0 {
                Relation::Untracked
            } else {
                return None;
            };
            let upstream = facts
                .config
                .branches
                .get(&b.branch.name)
                .and_then(BranchConfig::display);
            let on = checkouts_on(b, facts, sessions);
            let holds = Holds {
                pinned: entry.pinned,
                entry: entry_held,
                push_url,
                fetch_failed: facts.fetch_failed,
                on: &on,
                detection: sessions.detection,
            };
            // an alias never acts: git writes through it to its target,
            // unchecked (`BranchStatus::symref` says why nothing is lost)
            let verdict = if b.branch.symref.is_some() {
                Verdict::Quiet
            } else {
                // any branch may be checked out in a worktree no one can
                // see: deleting it would strand that worktree
                let unseen = worktrees_unread || on.head_unknown;
                match verdict(entry, b, relation, upstream.is_some(), &holds) {
                    Verdict::Cleanup { .. } if unseen && b.unique_commits > 0 => Verdict::LocalOnly,
                    Verdict::Cleanup { .. } if unseen => Verdict::Quiet,
                    verdict => verdict,
                }
            };
            Some(BranchStatus {
                name: b.branch.name.clone(),
                upstream,
                worktree: b.branch.worktree.clone(),
                symref: b.branch.symref.clone(),
                unique_commits: b.unique_commits,
                newest_commit_at: b.branch.committer_time,
                relation,
                verdict,
            })
        })
        .collect();
    let unprobed = facts
        .unprobed
        .iter()
        .map(|u| UnprobedWorktreeStatus {
            prune: prune(u, facts),
            busy: sessions.at(&u.path).to_vec(),
            worktree: u.clone(),
        })
        .collect();
    let at_rest = at_rest(
        entry.branch.as_deref(),
        &Primary {
            head: &facts.status.head,
            uncommitted: facts.status.uncommitted,
            in_progress: facts.in_progress,
        },
        tracked,
        &branches,
    );
    Classified {
        branches,
        needs_human,
        unprobed,
        at_rest,
    }
}

/// The primary checkout's state, as `at_rest` reads it.
#[derive(Debug, Clone, Copy)]
pub struct Primary<'a> {
    pub head: &'a Head,
    pub uncommitted: Uncommitted,
    pub in_progress: Option<InProgressOp>,
}

/// Whether the primary checkout is at rest where the registry puts it
/// (`AtRest`).
///
/// For an entry following `branch`, whose `branches` are classified —
/// compared against origin when `tracked` (owned, or a third-party
/// reference the run refreshes).
///
/// `on_branch` is `None` exactly when `branch` is; `followed` is the
/// relation `branches` carries for `branch`, and `None` when there's no
/// such branch or the entry isn't `tracked` — an untracked reference's
/// branches read `Untracked` for want of a comparison, a relation never
/// computed.
pub fn at_rest(
    branch: Option<&str>,
    primary: &Primary<'_>,
    tracked: bool,
    branches: &[BranchStatus],
) -> AtRest {
    AtRest {
        on_branch: branch
            .map(|branch| matches!(primary.head, Head::Branch { name } if name == branch)),
        clean: primary.uncommitted.is_clean(),
        idle: primary.in_progress.is_none(),
        followed: branch
            .filter(|_| tracked)
            .and_then(|branch| branches.iter().find(|b| b.name == branch))
            .map(|b| b.relation),
    }
}

/// A missing entry's clone verdict, and the reason a person decides it,
/// when one does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedMissing {
    pub clone: CloneVerdict,
    pub needs_human: Vec<NeedsHuman>,
}

/// Classifies an entry whose dir is missing: sync clones it
/// (`clone_recipe`), unless something at that path, or the registry, holds
/// it.
///
/// Another entry naming the same repo holds it for a person
/// (`clone_shares_repo`, `HeldBy::Entry`): the missing dir may have been a
/// worktree of that repo, and a second clone would be a guess. So does each
/// of `unregistered` — the unregistered scan's dirs, empty when it didn't
/// run — whose origin names the entry's repo, or a rename of it
/// (`cloned_unregistered`, `names_repo_loosely`): the entry's checkout
/// under another name, most likely. A live session
/// working at or under the missing path holds it (`busy`: its dir was
/// deleted from under it, and the clone would land where it works), as
/// does another entry's gone worktree recorded there (`recorded_worktree`:
/// that repo would take the clone for its worktree's files) — named in
/// that order. Nothing else holds a clone: it creates a dir and overwrites
/// nothing, so neither an agent running the tool, a pin (cloned, then held
/// for good), an archived repo, nor busy detection that's unavailable
/// holds it.
pub fn classify_missing(
    entry: &Entry,
    busy: bool,
    recorded_worktree: bool,
    unregistered: &[UnregisteredClone],
) -> ClassifiedMissing {
    let recipe = clone_recipe(entry);
    let needs_human: Vec<NeedsHuman> = entry
        .same_repo_as
        .iter()
        .map(|with| NeedsHuman::CloneSharesRepo { with: with.clone() })
        .chain(
            unregistered
                .iter()
                .filter(|u| clones_repo(u, &entry.url))
                .map(|u| NeedsHuman::ClonedUnregistered { dir: u.dir.clone() }),
        )
        .collect();
    let held = if !needs_human.is_empty() {
        Some(HeldBy::Entry)
    } else if busy {
        Some(HeldBy::Busy)
    } else if recorded_worktree {
        Some(HeldBy::UnprobedWorktree)
    } else {
        None
    };
    let clone = match held {
        Some(by) => CloneVerdict::Held { recipe, by },
        None => CloneVerdict::Act { recipe },
    };
    ClassifiedMissing { clone, needs_human }
}

/// Whether an unregistered dir holds a clone of `url`'s repo, or likely
/// does: its origin names it (`names_repo_loosely`), read past the `***`
/// the scan redacts a credential to (an origin with one names its repo all
/// the same). A clone's temp dir is the tool's own, a clone a sync didn't
/// finish or is still making, never a checkout under another name.
fn clones_repo(u: &UnregisteredClone, url: &RepoUrl) -> bool {
    u.kind != UnregisteredKind::UnfinishedClone
        && u.origin
            .as_deref()
            .is_some_and(|o| names_repo_loosely(&o.replacen("://***@", "://", 1), url))
}

/// Whether a remote URL names the registry's repo as `origin_matches`
/// reads it, or one renamed from or to it: the same host and account, and
/// a repo name equal once ASCII case is folded and `-` and `_` read as one
/// (`vscode_extension_tsv_format` for `vscode-extension-tsv-format`).
///
/// Only ever holds a clone for a person (`cloned_unregistered`): a false
/// match costs a question, never an action. Origin drift and push URLs
/// stay exact (`origin_matches`).
fn names_repo_loosely(origin: &str, url: &RepoUrl) -> bool {
    remote_parts(origin).is_some_and(|p| {
        let (account, name) = p.path.split_once('/').unwrap_or((p.path, ""));
        p.port.is_none()
            && p.host.eq_ignore_ascii_case(&url.host)
            && account.eq_ignore_ascii_case(&url.account)
            && repo_names_alike(name, &url.name)
    })
}

/// Two repo names equal with ASCII case folded and `_` read as `-`.
fn repo_names_alike(a: &str, b: &str) -> bool {
    let fold = |c: u8| {
        if c == b'_' {
            b'-'
        } else {
            c.to_ascii_lowercase()
        }
    };
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| fold(x) == fold(y))
}

/// How a missing entry is cloned: from `Entry::remote_url` (transport
/// follows write authority), on its branch when it names one, shallow and
/// sparse as a reference declares.
pub fn clone_recipe(entry: &Entry) -> CloneRecipe {
    CloneRecipe {
        url: entry.remote_url(),
        branch: entry.branch.clone(),
        shallow: entry.shallow,
        sparse: entry.sparse.clone(),
    }
}

/// An owned branch's relation to its origin upstream.
fn relation(b: &BranchFacts, facts: &RepoFacts) -> Relation {
    let origin = facts
        .config
        .branches
        .get(&b.branch.name)
        .is_some_and(BranchConfig::is_origin);
    if !origin {
        return Relation::Untracked;
    }
    if b.branch.upstream_ref.is_none() {
        return Relation::Unmapped;
    }
    match (b.branch.track, facts.layout.shallow) {
        (Track::Gone, _) => Relation::Gone,
        (Track::Even, _) => Relation::InSync,
        (_, true) if b.unique_commits > 0 && b.on_fetched_tip => Relation::Ahead {
            commits: b.unique_commits,
        },
        (_, true) => Relation::Shallow,
        (Track::Ahead(commits), false) => Relation::Ahead { commits },
        (Track::Behind(commits), false) => Relation::Behind { commits },
        (Track::Diverged { ahead, behind }, false) => Relation::Diverged { ahead, behind },
    }
}

/// Every checkout a branch is on, folded: git allows one branch on HEAD in
/// several (`worktree add -f`, `checkout --ignore-other-worktrees`), and any
/// one of them busy, dirty, or unknown holds it.
// Independent facts folded over the checkouts, not a hidden state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CheckoutsOn<'a> {
    /// On HEAD in some checkout — or possibly, in one whose HEAD is unknown.
    checked_out: bool,
    /// A live session works in one of them.
    busy: bool,
    /// A live session may work in one of them, unseen: its path couldn't be
    /// resolved, or it's a worktree git names that the probe didn't find, so
    /// no session was scoped to it. Or a live session works through a git
    /// dir no worktree list names that may be on it (`UnlistedGitDir`).
    maybe_busy: bool,
    dirty: bool,
    unprobed: bool,
    /// Possibly on HEAD in an unprobed worktree whose HEAD couldn't be read
    /// (its git dir unreadable, or its `HEAD`): it counts for every branch.
    head_unknown: bool,
    /// On HEAD in more than one checkout, counting unprobed ones and
    /// unlisted git dirs that may be on it.
    several: bool,
    /// The one checkout it's on, when that's a worktree `git worktree
    /// remove` would take: linked (never the main worktree), no submodules,
    /// not locked, no operation in progress, clean — and not a registry
    /// entry's dir, which is another entry's checkout to keep, nor one a
    /// live session works in, or might (busy detection unavailable, or its
    /// path unresolvable). An unprobed one is never removable here: a gone
    /// one's cleanup is its `prune`.
    removable: Option<&'a str>,
}

/// What may hold a branch's action back.
// Independent facts, each holding some actions, not a hidden state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
struct Holds<'a, 'b> {
    /// The entry is pinned.
    pinned: bool,
    /// An entry-level `needs_human` reason.
    entry: bool,
    /// A push through origin wouldn't reach the registry's repo.
    push_url: bool,
    /// The run's fetch of the entry failed or was refused.
    fetch_failed: bool,
    on: &'b CheckoutsOn<'a>,
    detection: Detection,
}

impl Holds<'_, '_> {
    /// What holds `action`, if anything: a pin (whose pushes never get
    /// here), an entry-level reason, a failed fetch, or a live session holds
    /// every action; a dirty checkout, one that couldn't be probed, or a
    /// branch on HEAD in several checkouts, all but a push, which only moves
    /// refs and which the remote checks; a checkout that may be busy — busy
    /// detection unavailable, which leaves every checkout in doubt, or one
    /// on the branch whose path can't be resolved or that the probe didn't
    /// find, or an unlisted git dir a session works through — holds every
    /// action; and a push URL other than the registry's holds a push. A pin
    /// names the hold before anything else, since clearing the rest never
    /// releases it; otherwise the most specific reason names it.
    fn of(&self, action: SyncAction) -> Option<HeldBy> {
        let push = matches!(action, SyncAction::Push { .. });
        if self.pinned {
            Some(HeldBy::Pinned)
        } else if self.entry {
            Some(HeldBy::Entry)
        } else if push && self.push_url {
            Some(HeldBy::PushUrl)
        } else if self.fetch_failed {
            Some(HeldBy::FetchFailed)
        } else if self.on.busy {
            Some(HeldBy::Busy)
        } else if !push && self.on.dirty {
            Some(HeldBy::DirtyCheckout)
        } else if !push && self.on.unprobed {
            Some(HeldBy::UnprobedWorktree)
        } else if !push && self.on.several {
            Some(HeldBy::SeveralCheckouts)
        } else if self.on.maybe_busy || self.detection == Detection::Unavailable {
            Some(HeldBy::BusyUnknown)
        } else {
            None
        }
    }
}

/// Folds every checkout whose HEAD is the branch — the primary, each probed
/// worktree, and each unprobed one on it; an unprobed one whose HEAD is
/// unknown might be on any branch, so it counts for all. Matched by name,
/// never by path: `%(worktreepath)` names only one checkout, git's paths are
/// resolved while the primary's is root-joined, and a symlinked workspace
/// root makes them differ. (Sessions, and checkouts that couldn't be
/// resolved, are looked up by the path each checkout's facts spell, which
/// is how `scope_sessions` keyed them — a session in an unprobed worktree's
/// files wherever they really are included, found by the `.git` it walks up
/// to.)
///
/// A branch git's `%(worktreepath)` says is checked out, but in no checkout
/// the probe found, may be busy: it's unprobed, and a session there was
/// scoped to nothing — the worktree list and `for-each-ref` race a worktree
/// being added. It holds every action, pushes included (`BusyUnknown`). A
/// bare repo's main worktree is the exception: git names it for its HEAD's
/// branch, but it has no files to hold. So does a git dir no worktree list
/// names that shares the refs, when a live session works through it and
/// its HEAD is on the branch or unknown.
fn checkouts_on<'a>(
    b: &BranchFacts,
    facts: &'a RepoFacts,
    sessions: &EntrySessions,
) -> CheckoutsOn<'a> {
    let name = b.branch.name.as_str();
    let on = |head: &Head| matches!(head, Head::Branch { name: n } if n == name);
    let busy = |path: &str| !sessions.at(path).is_empty();
    let maybe_busy = |path: &str| sessions.unresolved_at(path);
    let mut folded = CheckoutsOn::default();
    let mut count = 0;
    let mut removable = None;
    if on(&facts.status.head) {
        count += 1;
        folded.dirty |= !facts.status.uncommitted.is_clean();
        folded.busy |= busy(&facts.path);
        folded.maybe_busy |= maybe_busy(&facts.path);
    }
    for c in facts.worktrees.iter().filter(|c| on(&c.head)) {
        count += 1;
        folded.dirty |= !c.uncommitted.is_clean();
        folded.busy |= busy(&c.path);
        folded.maybe_busy |= maybe_busy(&c.path);
        let removable_here = c.linked
            && !facts.registry_worktrees.contains(&c.path)
            && c.submodules == Some(false)
            && !c.locked
            && c.in_progress.is_none()
            && c.uncommitted.is_clean()
            && !busy(&c.path)
            && !maybe_busy(&c.path)
            && sessions.detection == Detection::Available;
        if removable_here {
            removable = Some(c.path.as_str());
        }
    }
    let unprobed: Vec<&str> = facts
        .unprobed
        .iter()
        .filter(|u| match &u.head {
            UnprobedHead::Branch { name: n } => n == name,
            UnprobedHead::Detached { .. } => false,
            UnprobedHead::Unknown => true,
        })
        .map(|u| u.path.as_str())
        .collect();
    folded.head_unknown = facts
        .unprobed
        .iter()
        .any(|u| u.head == UnprobedHead::Unknown);
    if !unprobed.is_empty() {
        count += unprobed.len();
        folded.unprobed = true;
        folded.busy |= unprobed.iter().any(|p| busy(p));
        folded.maybe_busy |= unprobed.iter().any(|p| maybe_busy(p));
    }
    // a git dir no worktree list names, sharing the refs, a session in it
    let unlisted = sessions.unlisted_on(name);
    if unlisted > 0 {
        count += unlisted;
        folded.maybe_busy = true;
    }
    // git says it's checked out, but in no checkout it listed: unknown, and
    // where it is no session was scoped to
    let elsewhere = b
        .branch
        .worktree
        .as_deref()
        .is_some_and(|w| Some(w) != facts.bare_main.as_deref());
    if count == 0 && elsewhere {
        folded.unprobed = true;
        folded.maybe_busy = true;
    }
    folded.checked_out = count > 0 || b.branch.worktree.is_some();
    folded.several = count > 1;
    if count == 1 {
        folded.removable = removable;
    }
    folded
}

/// What sync does with a branch. `has_upstream` is whether any upstream is
/// configured; `holds` what may hold its action back, including the
/// checkouts it's on.
///
/// A pin is left alone — never fetched, updated, pushed, or reported
/// behind — so what its remote-tracking refs say of a branch is stale by
/// contract: a branch in any relation but a fast-forward's or a move's
/// reads `LocalOnly` when it has commits on no remote ref, else `Quiet`,
/// never a push, cleanup, or needs-human. Its fast-forwards and moves are
/// the pin's to hold.
fn verdict(
    entry: &Entry,
    b: &BranchFacts,
    relation: Relation,
    has_upstream: bool,
    holds: &Holds<'_, '_>,
) -> Verdict {
    use std::ops::ControlFlow::{Break, Continue};

    let on = holds.on;
    let action = match relation {
        // a third-party reference is read-only by derivation: never pushed,
        // so what's ahead is local work, or on another remote already
        Relation::Ahead { .. } if !entry.writable => Break(if b.unique_commits > 0 {
            Verdict::LocalOnly
        } else {
            Verdict::Quiet
        }),
        Relation::Ahead { .. } if entry.archived => Break(Verdict::NeedsHuman {
            reason: BranchNeedsHuman::ArchivedAhead,
        }),
        // the push would name it on origin: only a branch there
        Relation::Ahead { .. } if push_target(&b.branch).is_none() => Break(Verdict::NeedsHuman {
            reason: BranchNeedsHuman::UpstreamNotABranch,
        }),
        Relation::Ahead { commits } => Continue(SyncAction::Push { commits }),
        Relation::Behind { commits } => Continue(SyncAction::FastForward { commits }),
        // nothing local at stake: a stale pointer at an old root
        Relation::Shallow if b.unique_commits == 0 => Continue(SyncAction::Move),
        Relation::Shallow => Break(Verdict::NeedsHuman {
            reason: BranchNeedsHuman::ShallowLocalWork,
        }),
        Relation::Diverged { .. } => Break(Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Diverged,
        }),
        Relation::Unmapped => Break(Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Unmapped,
        }),
        // the branch the entry follows is never cleanup: its gone upstream
        // is the entry's `default_branch_gone` reason
        Relation::Gone if Some(b.branch.name.as_str()) == entry.branch.as_deref() => {
            Break(if b.unique_commits > 0 {
                Verdict::LocalOnly
            } else {
                Verdict::Quiet
            })
        }
        Relation::Gone => Break(Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: on.removable.map(str::to_owned),
        }),
        Relation::Untracked if b.unique_commits > 0 => Break(Verdict::LocalOnly),
        // nothing unique and no upstream: merged, unless it's checked out
        // anywhere, possibly (a fresh branch looks the same), or the
        // registry's branch (a needs-human reason) — so never in a worktree
        // to remove
        Relation::Untracked
            if !has_upstream
                && !on.checked_out
                && Some(b.branch.name.as_str()) != entry.branch.as_deref() =>
        {
            Break(Verdict::Cleanup {
                reason: CleanupReason::Merged,
                removable_worktree: None,
            })
        }
        // in sync; tracking another remote; checked out with nothing committed
        Relation::InSync | Relation::Untracked => Break(Verdict::Quiet),
    };
    let action = match action {
        // the tool leaves a pin alone, pushes included, so no stale ref's
        // word stands: the branch carries local work or nothing. Its other
        // actions are the pin's to hold
        Break(_) | Continue(SyncAction::Push { .. }) if entry.pinned => {
            return if b.unique_commits > 0 {
                Verdict::LocalOnly
            } else {
                Verdict::Quiet
            };
        }
        Break(verdict) => return verdict,
        Continue(action) => action,
    };
    holds
        .of(action)
        .map_or(Verdict::Act { action }, |by| Verdict::Held { action, by })
}

fn needs_human(
    entry: &Entry,
    facts: &RepoFacts,
    sessions: &EntrySessions,
    refresh: Refresh,
) -> Vec<NeedsHuman> {
    let mut reasons = Vec::new();
    // one per checkout with an operation mid-way, the primary's first, then
    // the other worktrees', probed or not
    let ops = std::iter::once((&facts.path, facts.in_progress))
        .chain(facts.worktrees.iter().map(|c| (&c.path, c.in_progress)))
        .chain(facts.unprobed.iter().map(|u| (&u.path, u.in_progress)));
    for (checkout, op) in ops {
        if let Some(op) = op {
            reasons.push(NeedsHuman::OperationInProgress {
                checkout: checkout.clone(),
                op,
            });
        }
    }
    // an operation there can't be ruled out
    for path in &facts.unreadable {
        reasons.push(NeedsHuman::WorktreeUnreadable { path: path.clone() });
    }
    let origin = origin_drift(entry, &facts.config);
    let drift = origin.is_some();
    if let Some(origin) = origin {
        reasons.push(NeedsHuman::OriginMismatch {
            origin,
            expected: entry.remote_url(),
            fix: OriginFix::decide(&facts.config),
        });
    }
    // an owned entry's fetch that would reach elsewhere, rewrites applied
    if !drift && let Some(fetch_url) = fetch_url_mismatch(entry, &facts.config) {
        let rewritten = facts.config.origin_url() != Some(fetch_url);
        reasons.push(NeedsHuman::FetchUrlMismatch {
            fetch_url: without_userinfo(fetch_url).into_owned(),
            expected: entry.remote_url(),
            fix: (!rewritten).then(|| OriginFix::decide(&facts.config)),
        });
    }
    // a refresh's fetch that wouldn't reach the repo over HTTPS
    if let (
        Some(RefreshVerdict::Held {
            by: HeldBy::OriginNotHttps,
        }),
        Some(fetch_url),
    ) = (
        refresh_verdict(entry, refresh, &facts.config),
        &facts.config.origin_fetch_url,
    ) {
        let rewritten = facts.config.origin_url() != Some(fetch_url.as_str());
        reasons.push(NeedsHuman::OriginNotHttps {
            fetch_url: without_userinfo(fetch_url).into_owned(),
            expected: entry.remote_url(),
            fix: (!rewritten).then(|| OriginFix::decide(&facts.config)),
        });
    }
    let head = &facts.status.head;
    // the branch the entry follows; a pin's checkout is its consumer's,
    // wherever its HEAD is, so nothing is expected of it
    if let (Some(branch), false) = (&entry.branch, entry.pinned) {
        let followed = facts.branches.iter().find(|b| b.branch.name == *branch);
        if followed.is_none() {
            reasons.push(NeedsHuman::DefaultBranchMissing {
                branch: branch.clone(),
            });
        } else if followed.is_some_and(|b| {
            tracked(entry, refresh, &facts.config) && relation(b, facts) == Relation::Gone
        }) {
            reasons.push(NeedsHuman::DefaultBranchGone {
                branch: branch.clone(),
            });
        } else if !facts
            .config
            .branches
            .get(branch)
            .is_some_and(BranchConfig::is_origin)
        {
            reasons.push(NeedsHuman::DefaultBranchNoUpstream {
                branch: branch.clone(),
            });
        }
        // a rebase or bisect detaches HEAD by design: the operation is
        // the reason, and reattaching mid-way would be the wrong fix; a
        // merge, cherry-pick, revert, sequencer, or am keeps HEAD on its
        // branch, so a detach beside one is still unexpected. Only the
        // primary's HEAD and operation count: a linked worktree detached
        // is normal, and its operation can't explain the primary's HEAD
        if matches!(head, Head::Detached { .. })
            && !matches!(
                facts.in_progress,
                Some(InProgressOp::Rebase | InProgressOp::Bisect)
            )
        {
            reasons.push(NeedsHuman::UnexpectedDetached {
                checkout: facts.path.clone(),
            });
        }
    }
    // in the order the checkouts are probed: the primary, then the other
    // worktrees, probed or not. Said once: a checkout at or under a git dir
    // that can't be read (an unlisted worktree whose admin dir can't be
    // looked up is that admin dir) is that reason's to name, and it holds
    // the entry already
    let unreadable = |checkout: &str| {
        facts
            .unreadable
            .iter()
            .any(|p| Path::new(checkout).starts_with(p))
    };
    let checkouts = std::iter::once(&facts.path)
        .chain(facts.worktrees.iter().map(|c| &c.path))
        .chain(facts.unprobed.iter().map(|u| &u.path))
        .filter(|c| !unreadable(c));
    for checkout in checkouts {
        if let Some(u) = sessions.unresolved.get(checkout) {
            reasons.push(NeedsHuman::CheckoutUnresolvable {
                checkout: checkout.clone(),
                path: u.path.clone(),
                error: u.error.clone(),
            });
        }
    }
    for (git_dir, u) in &sessions.unlisted {
        reasons.push(NeedsHuman::UnlistedGitDir {
            git_dir: git_dir.clone(),
            head: u.head.clone(),
            busy: u.busy.clone(),
        });
    }
    // said with a matching origin only: origin drift already holds the
    // entry, and its fix may fix this too
    if let Some(urls) = &facts.push_urls
        && !drift
        && !push_urls_match(urls, &entry.url)
    {
        reasons.push(NeedsHuman::PushUrlMismatch {
            push_urls: urls
                .iter()
                .map(|u| without_userinfo(u).into_owned())
                .collect(),
            expected: entry.remote_url(),
        });
    }
    reasons
}

/// `origin` as git sees it, when it isn't the registry's repo
/// (`origin_matches`): another URL, or none. `None` when it is.
pub fn origin_drift(entry: &Entry, config: &ConfigFacts) -> Option<OriginRemote> {
    match config.origin_url() {
        Some(url) if origin_matches(url, &entry.url) => None,
        Some(url) => Some(OriginRemote::Url {
            url: without_userinfo(url).into_owned(),
        }),
        None if config.origin_keys != OriginKeys::None => Some(OriginRemote::NoUrl),
        None => Some(OriginRemote::Missing),
    }
}

/// The ref a push of `b` through origin names: its upstream's ref on the
/// remote, when that's a branch.
///
/// `refs/heads/<name>`, never `refs/heads/HEAD` (which would create a
/// branch named `HEAD` there), and a ref name git accepts.
pub fn push_target(b: &RefFacts) -> Option<&str> {
    let merge = b.merge_ref.as_deref()?;
    let name = merge.strip_prefix("refs/heads/")?;
    (!name.is_empty() && name != "HEAD" && is_valid_refname(merge.as_bytes())).then_some(merge)
}

/// Whether a push through origin reaches the registry's repo (`url`), over
/// SSH.
///
/// Exactly one push URL, SSH (scp-like `git@host:path` or `ssh://`), naming
/// the registry's repo as `origin_matches` reads it. Several URLs would
/// each take the push.
pub fn push_urls_match(urls: &[String], url: &RepoUrl) -> bool {
    match urls {
        [one] => remote_parts(one).is_some_and(|p| p.ssh && names_repo(&p, url)),
        _ => false,
    }
}

/// Whether a remote URL names the registry's repo, read structurally
/// (`remote_parts`), never by its text.
///
/// The host git connects to is the registry's (ASCII case folded), with no
/// port — the registry's URLs name none — and the path on it is
/// `<account>/<name>` (a trailing `.git` or `/` dropped, case folded:
/// GitHub paths are case-insensitive). SSH, `git://`, and HTTPS forms
/// compare equal; a `user@` drops. Anything else — an `@` outside the
/// authority, an escape, an IP literal, a port — is a mismatch.
pub fn origin_matches(origin: &str, url: &RepoUrl) -> bool {
    remote_parts(origin).is_some_and(|p| names_repo(&p, url))
}

fn names_repo(p: &RemoteParts<'_>, url: &RepoUrl) -> bool {
    let (account, name) = p.path.split_once('/').unwrap_or((p.path, ""));
    p.port.is_none()
        && p.host.eq_ignore_ascii_case(&url.host)
        && account.eq_ignore_ascii_case(&url.account)
        && name.eq_ignore_ascii_case(&url.name)
}

/// The account a remote URL names, lowercased.
///
/// The first segment of the path on its host (`remote_parts`, any port
/// aside), when a name follows; `None` for a URL with no host and account,
/// such as a local path.
pub fn remote_account(url: &str) -> Option<String> {
    let p = remote_parts(url)?;
    let mut parts = p.path.split('/');
    let (Some(account), Some(name)) = (parts.next(), parts.next()) else {
        return None;
    };
    let named = |s: &str| !s.is_empty() && s != "." && s != "..";
    (named(account) && named(name)).then(|| account.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::*;
    use crate::porcelain::{ConfigFacts, OriginUrl, RefFacts, StatusFacts};
    use crate::registry::EntryKind;
    use crate::sessions::{Session, SessionSource};
    use crate::state::{Checkout, GitDirHolds, Layout, Uncommitted, UnprobedWhy, UnprobedWorktree};

    const NOW: u64 = 1_800_000_000;

    fn url(s: &str) -> RepoUrl {
        RepoUrl::try_from(s.to_owned()).unwrap()
    }

    /// Where a test entry's checkout lives and who moves its HEAD.
    #[derive(Debug, Clone, Copy)]
    enum Mode<'a> {
        Follow(&'a str),
        Pinned,
        /// Pinned, its checkout living on the branch.
        PinnedOn(&'a str),
        Head,
    }

    fn owned(mode: Mode<'_>) -> Entry {
        let (branch, pinned) = match mode {
            Mode::Follow(b) => (Some(b.to_owned()), false),
            Mode::Pinned => (None, true),
            Mode::PinnedOn(b) => (Some(b.to_owned()), true),
            Mode::Head => (None, false),
        };
        Entry {
            key: "app".into(),
            kind: EntryKind::Repo,
            dir: "app".into(),
            url: url("https://github.com/me/app"),
            writable: true,
            archived: false,
            visibility: None,
            ci: false,
            branch,
            pinned,
            shallow: false,
            sparse: None,
            same_repo_as: None,
        }
    }

    fn third_party(mode: Mode<'_>) -> Entry {
        Entry {
            url: url("https://github.com/them/lib"),
            writable: false,
            kind: EntryKind::Reference,
            ..owned(mode)
        }
    }

    /// A branch: name, configured upstream remote (`None` = no config), the
    /// resolved upstream, the track, unique commits.
    struct B<'a> {
        name: &'a str,
        remote: Option<&'a str>,
        resolved: bool,
        track: Track,
        unique: u32,
        on_tip: bool,
    }

    const fn b<'a>(name: &'a str, remote: Option<&'a str>, resolved: bool, track: Track) -> B<'a> {
        B {
            name,
            remote,
            resolved,
            track,
            unique: 0,
            on_tip: false,
        }
    }

    impl B<'_> {
        const fn unique(mut self, n: u32) -> Self {
            self.unique = n;
            self
        }
        const fn on_tip(mut self) -> Self {
            self.on_tip = true;
            self
        }
    }

    fn facts(head: Head, branches: &[B<'_>]) -> RepoFacts {
        let mut config = ConfigFacts {
            origin_urls: vec![OriginUrl::repo("git@github.com:me/app")],
            origin_keys: OriginKeys::InRepo,
            ..ConfigFacts::default()
        };
        for b in branches {
            if let Some(remote) = b.remote {
                config.branches.insert(
                    b.name.into(),
                    BranchConfig {
                        remote: Some(remote.into()),
                        merge: Some(format!("refs/heads/{}", b.name)),
                    },
                );
            }
        }
        RepoFacts {
            path: "/ws/app".into(),
            git_dir: PathBuf::from("/ws/app/.git"),
            common_dir: PathBuf::from("/ws/app/.git"),
            config,
            status: StatusFacts {
                head,
                uncommitted: Uncommitted::default(),
                stashes: 0,
            },
            in_progress: None,
            primary_linked: false,
            primary_locked: false,
            locks: Vec::new(),
            worktrees: Vec::new(),
            registry_worktrees: HashSet::new(),
            unprobed: Vec::new(),
            relative_gitdir: None,
            unreadable: Vec::new(),
            git_dirs: Vec::new(),
            bare_main: None,
            branches: branches
                .iter()
                .map(|b| BranchFacts {
                    branch: RefFacts {
                        name: b.name.into(),
                        oid: format!("c-{}", b.name),
                        symref: None,
                        upstream_ref: b.resolved.then(|| {
                            format!("refs/remotes/{}/{}", b.remote.unwrap_or("origin"), b.name)
                        }),
                        merge_ref: b.resolved.then(|| format!("refs/heads/{}", b.name)),
                        track: b.track,
                        worktree: None,
                        committer_time: NOW - 3600,
                    },
                    unique_commits: b.unique,
                    on_fetched_tip: b.on_tip,
                })
                .collect(),
            layout: Layout {
                shallow: false,
                sparse: false,
                partial_filter: None,
            },
            fetched_at: None,
            fetch_failed: false,
            push_urls: Some(vec!["git@github.com:me/app".into()]),
        }
    }

    fn on(name: &str) -> Head {
        Head::Branch { name: name.into() }
    }

    fn relations(entry: &Entry, f: &RepoFacts) -> Vec<(String, Relation)> {
        classify(entry, f, &EntrySessions::idle(), Refresh::Unasked)
            .branches
            .into_iter()
            .map(|b| (b.name, b.relation))
            .collect()
    }

    const O: Option<&str> = Some("origin");

    #[test]
    fn owned_relations_from_the_track() {
        let f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("ahead", O, true, Track::Ahead(2)).unique(2),
                b("behind", O, true, Track::Behind(3)),
                b(
                    "diverged",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 4,
                    },
                )
                .unique(1),
                b("gone", O, true, Track::Gone).unique(1),
                b("unmapped", O, false, Track::Even).unique(5),
                b("local", None, false, Track::Even).unique(1),
                b("merged", None, false, Track::Even),
                b("other", Some("upstream"), true, Track::Behind(9)),
            ],
        );
        let got = relations(&owned(Mode::Follow("main")), &f);
        let want = [
            ("main", Relation::InSync),
            ("ahead", Relation::Ahead { commits: 2 }),
            ("behind", Relation::Behind { commits: 3 }),
            (
                "diverged",
                Relation::Diverged {
                    ahead: 1,
                    behind: 4,
                },
            ),
            ("gone", Relation::Gone),
            ("unmapped", Relation::Unmapped),
            ("local", Relation::Untracked),
            ("merged", Relation::Untracked),
            ("other", Relation::Untracked),
        ];
        let want: Vec<_> = want.iter().map(|(n, r)| ((*n).to_owned(), *r)).collect();
        assert_eq!(got, want);
    }

    fn verdicts(entry: &Entry, f: &RepoFacts) -> Vec<(String, Verdict)> {
        classify(entry, f, &EntrySessions::idle(), Refresh::Unasked)
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect()
    }

    fn named(want: &[(&str, Verdict)]) -> Vec<(String, Verdict)> {
        want.iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect()
    }

    const fn act(action: SyncAction) -> Verdict {
        Verdict::Act { action }
    }

    const fn needs(reason: BranchNeedsHuman) -> Verdict {
        Verdict::NeedsHuman { reason }
    }

    #[test]
    fn owned_verdicts() {
        let mut f = facts(
            on("fresh"),
            &[
                b("main", None, false, Track::Even),
                b("ahead", O, true, Track::Ahead(2)).unique(2),
                b("behind", O, true, Track::Behind(3)),
                b(
                    "diverged",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 4,
                    },
                )
                .unique(1),
                b("gone", O, true, Track::Gone).unique(1),
                b("unmapped", O, false, Track::Even).unique(5),
                b("local", None, false, Track::Even).unique(1),
                b("merged", None, false, Track::Even),
                b("other", Some("upstream"), true, Track::Behind(9)),
                b("fresh", None, false, Track::Even),
            ],
        );
        f.branches[9].branch.worktree = Some("/ws/app".into());
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[
                // the registry's branch without an upstream is an entry
                // reason, not merged work
                ("main", Verdict::Quiet),
                ("ahead", act(SyncAction::Push { commits: 2 })),
                ("behind", act(SyncAction::FastForward { commits: 3 })),
                ("diverged", needs(BranchNeedsHuman::Diverged)),
                (
                    "gone",
                    Verdict::Cleanup {
                        reason: CleanupReason::UpstreamGone,
                        removable_worktree: None
                    }
                ),
                ("unmapped", needs(BranchNeedsHuman::Unmapped)),
                ("local", Verdict::LocalOnly),
                (
                    "merged",
                    Verdict::Cleanup {
                        reason: CleanupReason::Merged,
                        removable_worktree: None
                    }
                ),
                ("other", Verdict::Quiet),
                // checked out with nothing committed: a fresh branch
                ("fresh", Verdict::Quiet),
            ])
        );
    }

    #[test]
    fn entry_reasons_hold_every_action() {
        let branches = [
            b("main", O, true, Track::Ahead(1)).unique(1),
            b("feat", O, true, Track::Behind(2)),
            b("wip", None, false, Track::Even).unique(1),
        ];
        let held = [
            (
                "main",
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::Entry,
                },
            ),
            (
                "feat",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 2 },
                    by: HeldBy::Entry,
                },
            ),
            // not an action, so not held
            ("wip", Verdict::LocalOnly),
        ];
        let e = owned(Mode::Follow("main"));

        let mut drift = facts(on("main"), &branches);
        drift.config.origin_urls = vec![OriginUrl::repo("git@github.com:someone/app")];
        assert_eq!(verdicts(&e, &drift), named(&held));

        let mut rebasing = facts(on("main"), &branches);
        rebasing.in_progress = Some(InProgressOp::Rebase);
        assert_eq!(verdicts(&e, &rebasing), named(&held));

        // a branch-scoped reason leaves the other branches to sync
        let detached = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &branches,
        );
        assert_eq!(
            verdicts(&e, &detached)[..2],
            named(&[
                ("main", act(SyncAction::Push { commits: 1 })),
                ("feat", act(SyncAction::FastForward { commits: 2 })),
            ])
        );
    }

    /// A linked worktree at `path`, on `head`, clean.
    fn linked(path: &str, head: Head) -> Checkout {
        Checkout {
            path: path.into(),
            primary: false,
            head,
            uncommitted: Uncommitted::default(),
            in_progress: None,
            locked: false,
            linked: true,
            submodules: Some(false),
            busy: Vec::new(),
            working: Vec::new(),
        }
    }

    /// An unprobed worktree at `path`, on `branch`.
    fn unprobed(path: &str, branch: Option<&str>, why: UnprobedWhy) -> UnprobedWorktree {
        UnprobedWorktree {
            path: path.into(),
            git_dir: Some("/ws/app/.git/worktrees/wt".into()),
            head: branch.map_or_else(
                || UnprobedHead::Detached {
                    commit: "0123456789abcdef0123456789abcdef01234567".into(),
                },
                |name| UnprobedHead::Branch {
                    name: name.to_owned(),
                },
            ),
            locked: false,
            in_progress: None,
            why,
            holds: None,
        }
    }

    #[test]
    fn a_dirty_checkout_holds_all_but_a_push() {
        let ff = |commits| SyncAction::FastForward { commits };
        let held = |action, by| Verdict::Held { action, by };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(2)),
                b("other", O, true, Track::Behind(3)),
                b("linked", O, true, Track::Behind(1)),
                b("linked-ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        // `%(worktreepath)` is git's resolved path; the checkouts are matched
        // by the branch on their HEAD, never by path
        f.branches[0].branch.worktree = Some("/real/ws/app".into());
        f.branches[2].branch.worktree = Some("/real/ws/app-linked".into());
        f.branches[3].branch.worktree = Some("/real/ws/app-linked-2".into());
        f.worktrees = vec![
            linked("/ws/app-linked", on("linked")),
            linked("/ws/app-linked-2", on("linked-ahead")),
        ];
        let e = owned(Mode::Follow("main"));

        let clean = verdicts(&e, &f);
        assert_eq!(
            clean,
            named(&[
                ("main", act(ff(2))),
                ("other", act(ff(3))),
                // a clean linked worktree holds nothing
                ("linked", act(ff(1))),
                ("linked-ahead", act(SyncAction::Push { commits: 1 })),
            ])
        );

        f.status.uncommitted.unstaged = 1;
        let dirty = verdicts(&e, &f);
        assert_eq!(
            dirty[..3],
            named(&[
                ("main", held(ff(2), HeldBy::DirtyCheckout)),
                // not checked out: moves in place
                ("other", act(ff(3))),
                // its own checkout is clean
                ("linked", act(ff(1))),
            ])
        );

        // a dirty linked worktree holds its own branch's fast-forward, and a
        // push on a branch checked out in one still acts
        f.status.uncommitted.unstaged = 0;
        f.worktrees[0].uncommitted.untracked = 1;
        f.worktrees[1].uncommitted.staged = 1;
        assert_eq!(
            verdicts(&e, &f)[2..],
            named(&[
                ("linked", held(ff(1), HeldBy::DirtyCheckout)),
                ("linked-ahead", act(SyncAction::Push { commits: 1 })),
            ])
        );

        // ahead, checked out in the dirty primary: the push still acts
        f.status.uncommitted.unstaged = 1;
        f.branches[0].branch.track = Track::Ahead(2);
        f.branches[0].unique_commits = 2;
        assert_eq!(verdicts(&e, &f)[0].1, act(SyncAction::Push { commits: 2 }));

        // an entry-level reason outranks the checkout, and holds the push too
        f.in_progress = Some(InProgressOp::Merge);
        assert_eq!(
            verdicts(&e, &f)[0].1,
            held(SyncAction::Push { commits: 2 }, HeldBy::Entry)
        );
    }

    fn verdicts_with(entry: &Entry, f: &RepoFacts, sessions: &EntrySessions) -> Vec<Verdict> {
        classify(entry, f, sessions, Refresh::Unasked)
            .branches
            .into_iter()
            .map(|b| b.verdict)
            .collect()
    }

    /// Sessions in the checkouts at `paths`, one each.
    fn busy_at(paths: &[&str]) -> EntrySessions {
        let mut sessions = EntrySessions::idle();
        for (pid, path) in (1..).zip(paths) {
            sessions.busy.insert(
                (*path).to_owned(),
                vec![Session::at(
                    pid,
                    0,
                    (*path).to_owned(),
                    SessionSource::SessionFile,
                )],
            );
        }
        sessions
    }

    #[test]
    fn a_busy_checkout_holds_every_action_on_its_branches() {
        let ff = |commits| SyncAction::FastForward { commits };
        let push = |commits| SyncAction::Push { commits };
        let held = |action, by| Verdict::Held { action, by };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)).unique(1),
                b("linked", O, true, Track::Behind(2)),
                b("other", O, true, Track::Ahead(3)).unique(3),
            ],
        );
        f.worktrees = vec![linked("/ws/app-linked", on("linked"))];
        let e = owned(Mode::Follow("main"));
        assert_eq!(
            verdicts_with(&e, &f, &EntrySessions::idle()),
            [act(push(1)), act(ff(2)), act(push(3))]
        );

        // pushes included; a branch checked out nowhere acts
        let both = busy_at(&["/ws/app", "/ws/app-linked"]);
        assert_eq!(
            verdicts_with(&e, &f, &both),
            [
                held(push(1), HeldBy::Busy),
                held(ff(2), HeldBy::Busy),
                act(push(3)),
            ]
        );
        // a busy checkout outranks its dirt
        f.worktrees[0].uncommitted.unstaged = 1;
        assert_eq!(verdicts_with(&e, &f, &both)[1], held(ff(2), HeldBy::Busy));
        // a session only in the linked worktree leaves the primary's branch
        assert_eq!(
            verdicts_with(&e, &f, &busy_at(&["/ws/app-linked"]))[0],
            act(push(1))
        );
        // an entry-level reason outranks it
        f.in_progress = Some(InProgressOp::Merge);
        assert_eq!(
            verdicts_with(&e, &f, &both)[0],
            held(push(1), HeldBy::Entry)
        );
        f.in_progress = None;

        // an unprobed worktree whose HEAD is unknown may be on any branch:
        // a session there holds them all
        f.worktrees.clear();
        f.unprobed = vec![UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..unprobed("/ws/app-lost", None, UnprobedWhy::Missing)
        }];
        let c = classify(&e, &f, &busy_at(&["/ws/app-lost"]), Refresh::Unasked);
        assert_eq!(
            c.branches
                .iter()
                .map(|b| b.verdict.clone())
                .collect::<Vec<_>>(),
            [
                held(push(1), HeldBy::Busy),
                held(ff(2), HeldBy::Busy),
                held(push(3), HeldBy::Busy),
            ]
        );
        assert_eq!(c.unprobed[0].busy.len(), 1);
    }

    #[test]
    fn unavailable_detection_holds_every_action() {
        let push = |commits| SyncAction::Push { commits };
        let held = |action, by| Verdict::Held { action, by };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(2)),
                b("other", O, true, Track::Ahead(3)).unique(3),
                b("old", O, true, Track::Gone),
            ],
        );
        f.status.uncommitted.untracked = 1;
        let mut wt = linked("/ws/app-old", on("old"));
        wt.submodules = Some(false);
        f.worktrees = vec![wt];
        let e = owned(Mode::Follow("main"));
        let gone = |removable: Option<&str>| Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: removable.map(str::to_owned),
        };
        assert_eq!(
            verdicts_with(&e, &f, &EntrySessions::idle())[1..],
            [act(push(3)), gone(Some("/ws/app-old"))]
        );
        assert_eq!(
            verdicts_with(&e, &f, &EntrySessions::unavailable()),
            [
                // the checkout's own reason names the hold
                held(
                    SyncAction::FastForward { commits: 2 },
                    HeldBy::DirtyCheckout
                ),
                // checked out nowhere, held all the same
                held(push(3), HeldBy::BusyUnknown),
                // a session there can't be ruled out
                gone(None),
            ]
        );
        // a busy worktree isn't removable either
        assert_eq!(
            verdicts_with(&e, &f, &busy_at(&["/ws/app-old"]))[2],
            gone(None)
        );
    }

    #[test]
    fn an_unresolvable_checkout_holds_the_branches_checked_out_there() {
        use crate::busy::UnresolvedCheckout;
        let ff = |commits| SyncAction::FastForward { commits };
        let push = |commits| SyncAction::Push { commits };
        let held = |action, by| Verdict::Held { action, by };
        let unresolved = |paths: &[&str]| {
            let mut sessions = EntrySessions::idle();
            for path in paths {
                sessions.unresolved.insert(
                    (*path).to_owned(),
                    UnresolvedCheckout {
                        path: (*path).to_owned(),
                        error: "Permission denied (os error 13)".into(),
                    },
                );
            }
            sessions
        };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)).unique(1),
                b("locked", O, true, Track::Behind(2)),
                b("linked", O, true, Track::Ahead(3)).unique(3),
                b("other", O, true, Track::Ahead(4)).unique(4),
                b("old", O, true, Track::Gone),
            ],
        );
        f.worktrees = vec![
            linked("/ws/sealed/app-linked", on("linked")),
            linked("/ws/sealed/app-old", on("old")),
            linked(
                "/ws/sealed/app-detached",
                Head::Detached {
                    commit: "0123456789abcdef0123456789abcdef01234567".into(),
                },
            ),
        ];
        f.unprobed = vec![unprobed(
            "/ws/sealed/app-locked",
            Some("locked"),
            UnprobedWhy::Failed {
                error: "checking /ws/sealed/app-locked/.git: Permission denied (os error 13)"
                    .into(),
            },
        )];
        let e = owned(Mode::Follow("main"));
        let gone = |removable: Option<&str>| Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: removable.map(str::to_owned),
        };
        assert_eq!(
            verdicts_with(&e, &f, &EntrySessions::idle()),
            [
                act(push(1)),
                // unprobed: its fast-forward held, not a push
                held(ff(2), HeldBy::UnprobedWorktree),
                act(push(3)),
                act(push(4)),
                gone(Some("/ws/sealed/app-old")),
            ]
        );

        // every worktree unresolvable, in the order the checkouts are
        // probed: each holds what's checked out there, pushes included, and
        // the most specific reason names the hold; the branches checked out
        // nowhere, and the detached worktree's HEAD, hold nothing
        let all = unresolved(&[
            "/ws/sealed/app-locked",
            "/ws/sealed/app-detached",
            "/ws/sealed/app-old",
            "/ws/sealed/app-linked",
        ]);
        let c = classify(&e, &f, &all, Refresh::Unasked);
        let reason = |checkout: &str| NeedsHuman::CheckoutUnresolvable {
            checkout: checkout.into(),
            path: checkout.into(),
            error: "Permission denied (os error 13)".into(),
        };
        assert_eq!(
            c.needs_human,
            [
                reason("/ws/sealed/app-linked"),
                reason("/ws/sealed/app-old"),
                reason("/ws/sealed/app-detached"),
                reason("/ws/sealed/app-locked"),
            ]
        );
        assert!(!c.needs_human.iter().any(NeedsHuman::holds_entry));
        assert_eq!(
            c.branches
                .into_iter()
                .map(|b| b.verdict)
                .collect::<Vec<_>>(),
            [
                act(push(1)),
                held(ff(2), HeldBy::UnprobedWorktree),
                held(push(3), HeldBy::BusyUnknown),
                act(push(4)),
                // a session there can't be ruled out
                gone(None),
            ]
        );
        // the primary too, alone
        assert_eq!(
            verdicts_with(&e, &f, &unresolved(&["/ws/app"])),
            [
                held(push(1), HeldBy::BusyUnknown),
                held(ff(2), HeldBy::UnprobedWorktree),
                act(push(3)),
                act(push(4)),
                gone(Some("/ws/sealed/app-old")),
            ]
        );
        // an unprobed worktree whose HEAD is unknown may be on any branch
        f.unprobed[0].head = UnprobedHead::Unknown;
        assert_eq!(
            verdicts_with(&e, &f, &unresolved(&["/ws/sealed/app-locked"])),
            [
                held(push(1), HeldBy::BusyUnknown),
                held(ff(2), HeldBy::UnprobedWorktree),
                held(push(3), HeldBy::BusyUnknown),
                held(push(4), HeldBy::BusyUnknown),
                // it may be on `old` too: deleting `old` could strand it
                Verdict::Quiet,
            ]
        );
    }

    #[test]
    fn a_worktree_that_was_not_probed_holds_all_but_a_push() {
        // git says each is checked out, but no probed checkout has them on
        // HEAD: the worktree's dir is gone, or its probe failed
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("gone-wt", O, true, Track::Behind(1)),
                b("gone-wt-ahead", O, true, Track::Ahead(1)).unique(1),
                b("failed-wt-ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        f.branches[1].branch.worktree = Some("/ws/app-gone".into());
        f.branches[2].branch.worktree = Some("/ws/app-gone-2".into());
        f.branches[3].branch.worktree = Some("/ws/app-failed".into());
        f.unprobed = vec![
            unprobed("/ws/app-gone", Some("gone-wt"), UnprobedWhy::Prunable),
            unprobed(
                "/ws/app-gone-2",
                Some("gone-wt-ahead"),
                UnprobedWhy::Missing,
            ),
            unprobed(
                "/ws/app-failed",
                Some("failed-wt-ahead"),
                UnprobedWhy::Failed {
                    error: "boom".into(),
                },
            ),
        ];
        // a probed linked worktree on another branch doesn't count
        f.worktrees = vec![linked("/ws/app-other", on("main-2"))];
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[
                ("main", Verdict::Quiet),
                (
                    "gone-wt",
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 1 },
                        by: HeldBy::UnprobedWorktree,
                    }
                ),
                // a push only moves refs: a session in the worktree's files,
                // wherever they are, would have made it busy
                ("gone-wt-ahead", act(SyncAction::Push { commits: 1 })),
                ("failed-wt-ahead", act(SyncAction::Push { commits: 1 })),
            ])
        );
    }

    #[test]
    fn an_unprobed_worktree_holds_its_push_only_when_busy() {
        // gone, moved by hand, on media mounted elsewhere, or named by no
        // path: wherever its files are, a session in them is attributed to
        // it by the `.git` it finds — so with none there, its push acts
        let push = |commits| SyncAction::Push { commits };
        let held = |by| Verdict::Held {
            action: push(1),
            by,
        };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)).unique(1),
                b("moved", O, true, Track::Ahead(1)).unique(1),
                b("usb", O, true, Track::Ahead(1)).unique(1),
                b("nameless", O, true, Track::Ahead(1)).unique(1),
                b("busy", O, true, Track::Ahead(1)).unique(1),
                b("old", O, true, Track::Gone),
            ],
        );
        f.unprobed = vec![
            unprobed("/ws/app-moved", Some("moved"), UnprobedWhy::Prunable),
            unprobed("/media/usb/app", Some("usb"), UnprobedWhy::Missing),
            // its `gitdir` unreadable: the path is its own git dir
            unprobed(
                "/ws/app/.git/worktrees/n",
                Some("nameless"),
                UnprobedWhy::Failed { error: "x".into() },
            ),
            // a session in its files: attributed, wherever they are
            unprobed("/ws/app-busy", Some("busy"), UnprobedWhy::Prunable),
            unprobed("/ws/app-old", Some("old"), UnprobedWhy::Prunable),
        ];
        let e = owned(Mode::Follow("main"));
        let classified = classify(&e, &f, &busy_at(&["/ws/app-busy"]), Refresh::Unasked);
        let verdicts: Vec<(String, Verdict)> = classified
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect();
        assert_eq!(
            verdicts,
            named(&[
                ("main", act(push(1))),
                ("moved", act(push(1))),
                ("usb", act(push(1))),
                ("nameless", act(push(1))),
                ("busy", held(HeldBy::Busy)),
                // cleanup isn't an action, and a gone worktree is never the
                // one to remove: its own cleanup is its prune, kept
                (
                    "old",
                    Verdict::Cleanup {
                        reason: CleanupReason::UpstreamGone,
                        removable_worktree: None,
                    }
                ),
            ])
        );
        let prunes: Vec<Option<Prune>> = classified.unprobed.into_iter().map(|u| u.prune).collect();
        assert_eq!(
            prunes,
            [
                Some(Prune::Safe),
                None,
                None,
                Some(Prune::Safe),
                Some(Prune::Safe),
            ]
        );

        // its HEAD unknown too: a session attributed to it holds every
        // branch, the primary's included
        f.unprobed = vec![UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..unprobed(
                "/ws/app/.git/worktrees/n",
                None,
                UnprobedWhy::Failed { error: "x".into() },
            )
        }];
        assert_eq!(
            verdicts_with(&e, &f, &EntrySessions::idle())[..5],
            [
                act(push(1)),
                act(push(1)),
                act(push(1)),
                act(push(1)),
                act(push(1)),
            ]
        );
        assert_eq!(
            verdicts_with(&e, &f, &busy_at(&["/ws/app/.git/worktrees/n"]))[..5],
            [
                held(HeldBy::Busy),
                held(HeldBy::Busy),
                held(HeldBy::Busy),
                held(HeldBy::Busy),
                held(HeldBy::Busy),
            ]
        );
    }

    #[test]
    fn unprobed_worktrees_hold_by_name_and_git_is_trusted_for_the_rest() {
        let held = Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::UnprobedWorktree,
        };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("listed", O, true, Track::Behind(1)),
                b("unlisted", O, true, Track::Behind(1)),
                b("unlisted-ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        // named only by the worktree list: `%(worktreepath)` is empty
        f.unprobed = vec![unprobed(
            "/ws/app-failed",
            Some("listed"),
            UnprobedWhy::Failed {
                error: "boom".into(),
            },
        )];
        // named only by `%(worktreepath)`: fail closed — and a session
        // there is scoped to no checkout, so it may be busy
        f.branches[2].branch.worktree = Some("/ws/app-somewhere".into());
        f.branches[3].branch.worktree = Some("/ws/app-elsewhere".into());
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f)[1..],
            named(&[
                ("listed", held.clone()),
                ("unlisted", held),
                (
                    "unlisted-ahead",
                    Verdict::Held {
                        action: SyncAction::Push { commits: 1 },
                        by: HeldBy::BusyUnknown,
                    }
                ),
            ])
        );

        // a bare repo's main worktree, which git names for its HEAD's
        // branch, has no files: nothing there to hold
        f.bare_main = Some("/ws/app-elsewhere".into());
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f)[3],
            (
                "unlisted-ahead".to_owned(),
                act(SyncAction::Push { commits: 1 })
            )
        );
    }

    #[test]
    fn a_worktree_whose_head_is_unknown_holds_every_fast_forward() {
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(1)),
                b("ahead", O, true, Track::Ahead(1)).unique(1),
                b("gone", O, true, Track::Gone),
                b("merged", None, false, Track::Even),
            ],
        );
        f.branches[2].branch.worktree = Some("/ws/app-gone".into());
        f.worktrees = vec![linked("/ws/app-gone", on("gone"))];
        f.unprobed = vec![UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..unprobed(
                "/ws/app/.git/worktrees/x",
                None,
                UnprobedWhy::Failed {
                    error: "not listed by git".into(),
                },
            )
        }];
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[
                (
                    "main",
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 1 },
                        by: HeldBy::UnprobedWorktree,
                    }
                ),
                // a push only moves refs
                ("ahead", act(SyncAction::Push { commits: 1 })),
                // it might be checked out there too: deleting it could
                // strand that worktree, so it's no cleanup
                ("gone", Verdict::Quiet),
                // it might be checked out there: a fresh branch, not merged
                ("merged", Verdict::Quiet),
            ])
        );
    }

    #[test]
    fn an_unreadable_git_dir_holds_the_entry_pushes_too() {
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(1)),
                b("ahead", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        f.unreadable = vec!["/ws/app/.git/worktrees".into()];
        let c = classify(
            &owned(Mode::Follow("main")),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        );
        assert_eq!(
            c.needs_human,
            [NeedsHuman::WorktreeUnreadable {
                path: "/ws/app/.git/worktrees".into()
            }]
        );
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[
                (
                    "main",
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 1 },
                        by: HeldBy::Entry,
                    }
                ),
                (
                    "ahead",
                    Verdict::Held {
                        action: SyncAction::Push { commits: 1 },
                        by: HeldBy::Entry,
                    }
                ),
            ])
        );
    }

    #[test]
    fn what_a_prune_would_lose() {
        let f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("feat", O, true, Track::Even),
            ],
        );
        let gone = unprobed("/ws/app-gone", Some("feat"), UnprobedWhy::Prunable);
        let loses = |losses| Some(Prune::Loses { losses });
        // on a branch that exists, at rest: nothing
        assert_eq!(prune(&gone, &f), Some(Prune::Safe));
        // a merge keeps HEAD on its branch: the operation alone is lost
        let merging = UnprobedWorktree {
            in_progress: Some(InProgressOp::Merge),
            ..gone.clone()
        };
        assert_eq!(
            prune(&merging, &f),
            loses(vec![PruneLoss::Operation {
                op: InProgressOp::Merge
            }])
        );
        let detached = unprobed("/ws/app-gone", None, UnprobedWhy::Prunable);
        assert_eq!(prune(&detached, &f), loses(vec![PruneLoss::DetachedHead]));
        let unknown = UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..gone.clone()
        };
        assert_eq!(prune(&unknown, &f), loses(vec![PruneLoss::UnknownHead]));
        // its branch deleted: its HEAD may be the only ref to its commit
        let orphaned = unprobed("/ws/app-gone", Some("deleted"), UnprobedWhy::Prunable);
        assert_eq!(
            prune(&orphaned, &f),
            loses(vec![PruneLoss::MissingBranch {
                name: "deleted".into()
            }])
        );
        // every loss, listed
        let both = UnprobedWorktree {
            in_progress: Some(InProgressOp::Rebase),
            ..detached
        };
        assert_eq!(
            prune(&both, &f),
            loses(vec![
                PruneLoss::Operation {
                    op: InProgressOp::Rebase
                },
                PruneLoss::DetachedHead
            ])
        );
        // a git dir that can't be matched can't be read
        let unmatched = UnprobedWorktree {
            git_dir: None,
            ..gone.clone()
        };
        assert_eq!(
            prune(&unmatched, &f),
            loses(vec![PruneLoss::UnmatchedGitDir])
        );
        // what its git dir alone holds
        let holding = |submodules, worktree_refs, staged| UnprobedWorktree {
            holds: Some(GitDirHolds {
                submodules,
                worktree_refs,
                staged,
            }),
            ..gone.clone()
        };
        assert_eq!(
            prune(&holding(false, false, Some(false)), &f),
            Some(Prune::Safe)
        );
        assert_eq!(
            prune(&holding(true, true, Some(true)), &f),
            loses(vec![
                PruneLoss::Submodules,
                PruneLoss::WorktreeRefs,
                PruneLoss::StagedChanges
            ])
        );
        // an index that couldn't be compared counts as staged...
        assert_eq!(
            prune(&holding(false, false, None), &f),
            loses(vec![PruneLoss::StagedChanges])
        );
        // ...unless its HEAD is already lost, which says as much
        let lost_head = UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..holding(false, false, None)
        };
        assert_eq!(prune(&lost_head, &f), loses(vec![PruneLoss::UnknownHead]));
        let lost_branch = UnprobedWorktree {
            head: UnprobedHead::Branch {
                name: "deleted".into(),
            },
            ..holding(false, false, None)
        };
        assert_eq!(
            prune(&lost_branch, &f),
            loses(vec![PruneLoss::MissingBranch {
                name: "deleted".into()
            }])
        );
        // a relative `gitdir` anywhere in the repo: no path is certain, so
        // nothing is safe
        let relative = RepoFacts {
            relative_gitdir: Some(PathBuf::from("/ws/app/.git/worktrees/k")),
            ..f.clone()
        };
        assert_eq!(
            prune(&gone, &relative),
            loses(vec![PruneLoss::RelativeGitdir {
                git_dir: "/ws/app/.git/worktrees/k".into()
            }])
        );
        // not gone: no prune at all
        for why in [
            UnprobedWhy::Missing,
            UnprobedWhy::Failed { error: "x".into() },
        ] {
            let u = UnprobedWorktree {
                why,
                ..gone.clone()
            };
            assert_eq!(prune(&u, &f), None);
        }
    }

    #[test]
    fn every_checkout_on_a_branch_counts() {
        // git allows one branch on HEAD in several checkouts
        // (`worktree add -f`); any dirty one holds it, and clean ones hold
        // its ff too: moving it in one would strand the others' HEAD
        let ff1 = Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::SeveralCheckouts,
        };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(1)),
                b("twice", O, true, Track::Gone),
            ],
        );
        f.branches[0].branch.worktree = Some("/ws/app".into());
        f.branches[1].branch.worktree = Some("/ws/app-twice-1".into());
        f.worktrees = vec![
            linked("/ws/app-main", on("main")),
            linked("/ws/app-twice-1", on("twice")),
            linked("/ws/app-twice-2", on("twice")),
        ];
        let e = owned(Mode::Follow("main"));
        let v = verdicts(&e, &f);
        assert_eq!(v[0].1, ff1);
        // two clean worktrees on it: neither is the branch's to remove
        assert_eq!(
            v[1].1,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None,
            }
        );

        // a clean primary doesn't hide a dirty worktree on the same branch
        f.worktrees[0].uncommitted.unstaged = 1;
        assert_eq!(
            verdicts(&e, &f)[0].1,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 1 },
                by: HeldBy::DirtyCheckout,
            }
        );
        // nor does a clean worktree hide an unprobed one
        f.worktrees[0].uncommitted.unstaged = 0;
        f.unprobed = vec![unprobed(
            "/ws/app-main-2",
            Some("main"),
            UnprobedWhy::Missing,
        )];
        assert_eq!(
            verdicts(&e, &f)[0].1,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 1 },
                by: HeldBy::UnprobedWorktree,
            }
        );
        // dirty outranks unknown
        f.status.uncommitted.untracked = 1;
        assert_eq!(
            verdicts(&e, &f)[0].1,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 1 },
                by: HeldBy::DirtyCheckout,
            }
        );
        // on one checkout, clean: the ff acts
        f.status.uncommitted.untracked = 0;
        f.unprobed.clear();
        f.worktrees.remove(0);
        assert_eq!(
            verdicts(&e, &f)[0].1,
            Verdict::Act {
                action: SyncAction::FastForward { commits: 1 },
            }
        );
    }

    #[test]
    fn a_failed_fetch_holds_every_action() {
        let ff = |commits| SyncAction::FastForward { commits };
        let held = |action, by| Verdict::Held { action, by };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(2)),
                b("ahead", O, true, Track::Ahead(1)),
                b("idle", O, true, Track::Behind(1)),
            ],
        );
        f.fetch_failed = true;
        let e = owned(Mode::Follow("main"));
        assert_eq!(
            verdicts(&e, &f),
            named(&[
                ("main", held(ff(2), HeldBy::FetchFailed)),
                (
                    "ahead",
                    held(SyncAction::Push { commits: 1 }, HeldBy::FetchFailed)
                ),
                ("idle", held(ff(1), HeldBy::FetchFailed)),
            ])
        );
        // an entry-level reason outranks it
        f.in_progress = Some(InProgressOp::Merge);
        assert_eq!(verdicts(&e, &f)[0].1, held(ff(2), HeldBy::Entry));
    }

    #[test]
    fn a_worktree_is_removable_only_when_git_would_remove_it() {
        let cleanup = |removable_worktree: Option<&str>| Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: removable_worktree.map(str::to_owned),
        };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("plain", O, true, Track::Gone),
                b("locked", O, true, Track::Gone),
                b("picking", O, true, Track::Gone),
                b("in-main", O, true, Track::Gone),
                b("with-submodules", O, true, Track::Gone),
                b("unchecked", O, true, Track::Gone),
            ],
        );
        let mut locked = linked("/ws/app-locked", on("locked"));
        locked.locked = true;
        let mut picking = linked("/ws/app-picking", on("picking"));
        picking.in_progress = Some(InProgressOp::CherryPick);
        // the main worktree, when the registry's dir is a linked one
        let mut main_wt = linked("/ws/app-main", on("in-main"));
        main_wt.linked = false;
        let mut submodules = linked("/ws/app-subs", on("with-submodules"));
        submodules.submodules = Some(true);
        // clean, unlocked, linked, but its submodules weren't checked
        let mut unchecked = linked("/ws/app-unchecked", on("unchecked"));
        unchecked.submodules = None;
        f.worktrees = vec![
            linked("/ws/app-plain", on("plain")),
            locked,
            picking,
            main_wt,
            submodules,
            unchecked,
        ];
        for i in 1..7 {
            f.branches[i].branch.worktree = Some(f.worktrees[i - 1].path.clone());
        }
        let v = verdicts(&owned(Mode::Follow("main")), &f);
        assert_eq!(
            v[1..],
            named(&[
                ("plain", cleanup(Some("/ws/app-plain"))),
                // `git worktree remove` refuses a locked worktree
                ("locked", cleanup(None)),
                ("picking", cleanup(None)),
                // `git worktree remove` refuses the main worktree, and one
                // with initialized submodules
                ("in-main", cleanup(None)),
                ("with-submodules", cleanup(None)),
                // unknown is not "none"
                ("unchecked", cleanup(None)),
            ])
        );
    }

    #[test]
    fn a_gone_branch_in_a_clean_linked_worktree_is_removable() {
        let cleanup = |removable_worktree: Option<&str>| Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree: removable_worktree.map(str::to_owned),
        };
        let mut f = facts(
            on("in-primary"),
            &[
                b("main", O, true, Track::Even),
                b("in-clean", O, true, Track::Gone).unique(1),
                b("in-dirty", O, true, Track::Gone),
                b("in-primary", O, true, Track::Gone),
                b("in-none", O, true, Track::Gone),
                b("in-unprobed", O, true, Track::Gone),
                b("merged-in-wt", None, false, Track::Even),
            ],
        );
        for (i, path) in [
            (1, "/ws/app-clean"),
            (2, "/ws/app-dirty"),
            (3, "/ws/app"),
            (5, "/ws/app-unprobed"),
            (6, "/ws/app-merged"),
        ] {
            f.branches[i].branch.worktree = Some(path.into());
        }
        let mut dirty = linked("/ws/app-dirty", on("in-dirty"));
        dirty.uncommitted.unstaged = 1;
        f.worktrees = vec![
            linked("/ws/app-clean", on("in-clean")),
            dirty,
            linked("/ws/app-merged", on("merged-in-wt")),
        ];
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[
                ("main", Verdict::Quiet),
                ("in-clean", cleanup(Some("/ws/app-clean"))),
                // not removable: its dirt shows as uncommitted instead
                ("in-dirty", cleanup(None)),
                // the primary checkout is never removable
                ("in-primary", cleanup(None)),
                ("in-none", cleanup(None)),
                ("in-unprobed", cleanup(None)),
                // nothing unique and checked out reads as a fresh branch
                ("merged-in-wt", Verdict::Quiet),
            ])
        );
    }

    #[test]
    fn a_push_url_other_than_the_registrys_holds_pushes_only() {
        let push = SyncAction::Push { commits: 1 };
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)),
                b("feat", O, true, Track::Behind(1)),
            ],
        );
        let e = owned(Mode::Follow("main"));
        let reasons = |f: &RepoFacts| {
            classify(&e, f, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .into_iter()
                .filter(|r| matches!(r, NeedsHuman::PushUrlMismatch { .. }))
                .collect::<Vec<_>>()
        };
        // the registry's repo over SSH, however spelled
        for url in [
            "git@github.com:me/app",
            "ssh://git@github.com/me/app.git",
            "git+ssh://git@github.com/Me/App/",
        ] {
            f.push_urls = Some(vec![url.into()]);
            assert_eq!(reasons(&f), [], "{url}");
            assert_eq!(verdicts(&e, &f)[0].1, act(push), "{url}");
        }
        // another repo, another transport, several URLs, or none
        for urls in [
            &["git@github.com:me/other"][..],
            &["git@evil.example.com:me/app"],
            &["https://github.com/me/app"],
            &["https://tok@github.com/me/app"],
            &["file:///srv/me/app.git"],
            &["/srv/me/app"],
            &["git@github.com:me/app", "git@github.com:me/app"],
            &[],
        ] {
            f.push_urls = Some(urls.iter().map(|u| (*u).to_owned()).collect());
            assert_eq!(
                reasons(&f),
                [NeedsHuman::PushUrlMismatch {
                    push_urls: urls.iter().map(|u| u.replace("tok@", "***@")).collect(),
                    expected: "git@github.com:me/app".into(),
                }],
                "{urls:?}"
            );
            assert_eq!(
                verdicts(&e, &f),
                named(&[
                    (
                        "main",
                        Verdict::Held {
                            action: push,
                            by: HeldBy::PushUrl
                        }
                    ),
                    ("feat", act(SyncAction::FastForward { commits: 1 })),
                ]),
                "{urls:?}"
            );
        }
        // a lookalike, however much of the registry's URL it spells
        for lookalike in LOOKALIKE_URLS {
            assert!(
                !push_urls_match(&[lookalike.to_owned()], &e.url),
                "{lookalike}"
            );
            f.push_urls = Some(vec![lookalike.to_owned()]);
            assert_eq!(
                verdicts(&e, &f)[0].1,
                Verdict::Held {
                    action: push,
                    by: HeldBy::PushUrl
                },
                "{lookalike}"
            );
        }
        // origin drift says it first, and holds the entry
        f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/other")];
        assert_eq!(reasons(&f), []);
        // not read: nothing to say
        f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/app")];
        f.push_urls = None;
        assert_eq!(reasons(&f), []);
    }

    #[test]
    fn a_push_names_only_a_branch_on_origin() {
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)),
                b("tip", O, true, Track::Ahead(2)),
                b("tag", O, true, Track::Ahead(1)),
                b("lag", O, true, Track::Behind(1)),
            ],
        );
        // `origin/HEAD` as an upstream, and a ref outside `refs/heads/`
        f.branches[1].branch.merge_ref = Some("refs/heads/HEAD".into());
        f.branches[2].branch.merge_ref = Some("refs/tags/v1".into());
        // behind: a fast-forward names no ref on origin
        f.branches[3].branch.merge_ref = Some("refs/heads/HEAD".into());
        assert_eq!(push_target(&f.branches[0].branch), Some("refs/heads/main"));
        assert_eq!(push_target(&f.branches[1].branch), None);
        assert_eq!(push_target(&f.branches[2].branch), None);
        // a ref name git itself refuses: never named on the remote
        let mut odd = f.branches[0].branch.clone();
        for merge in [
            "refs/heads/a..b",
            "refs/heads/x.lock",
            "refs/heads/a b",
            "refs/heads/a:b",
            "refs/heads/.hidden",
            "refs/heads/a//b",
            "refs/heads/a/",
            "refs/heads/a@{1}",
            "refs/heads/end.",
        ] {
            odd.merge_ref = Some(merge.into());
            assert_eq!(push_target(&odd), None, "{merge}");
        }
        odd.merge_ref = Some("refs/heads/feat/x-1.2".into());
        assert_eq!(push_target(&odd), Some("refs/heads/feat/x-1.2"));
        let e = owned(Mode::Follow("main"));
        assert_eq!(
            verdicts(&e, &f),
            named(&[
                ("main", act(SyncAction::Push { commits: 1 })),
                ("tip", needs(BranchNeedsHuman::UpstreamNotABranch)),
                ("tag", needs(BranchNeedsHuman::UpstreamNotABranch)),
                ("lag", act(SyncAction::FastForward { commits: 1 })),
            ])
        );
        // archived says it first
        let archived = Entry {
            archived: true,
            ..e
        };
        assert_eq!(
            verdicts(&archived, &f)[1].1,
            needs(BranchNeedsHuman::ArchivedAhead)
        );
    }

    #[test]
    fn a_refresh_is_asked_of_third_party_references_and_refused_by_pins() {
        use RefreshVerdict::{Act, Held};
        let refused = Some(Held { by: HeldBy::Pinned });
        // (entry, unasked, named, --references)
        let table = [
            (owned(Mode::Follow("main")), None, None, None),
            (owned(Mode::PinnedOn("fork")), None, refused, None),
            (third_party(Mode::Head), None, Some(Act), Some(Act)),
            (third_party(Mode::Pinned), None, refused, None),
        ];
        for (e, unasked, named, references) in table {
            // its origin the registry's repo, the verdict is the intent
            let config = ConfigFacts {
                origin_urls: vec![OriginUrl::repo(&e.remote_url())],
                origin_keys: OriginKeys::InRepo,
                origin_fetch_url: Some(e.remote_url()),
                ..ConfigFacts::default()
            };
            for (refresh, want) in [
                (Refresh::Unasked, unasked),
                (Refresh::Named, named),
                (Refresh::References, references),
            ] {
                assert_eq!(refresh_intent(&e, refresh), want, "{e:?} {refresh:?}");
                assert_eq!(
                    refresh_verdict(&e, refresh, &config),
                    want,
                    "{e:?} {refresh:?}"
                );
            }
        }
    }

    /// A refresh of a repo whose origin isn't the registry's repo is held
    /// for its origin drift, never fetched: another URL (an SSH fork, an
    /// HTTPS fork), an origin with no URL, or none. A pin named stays
    /// refused by its pin; an owned entry is never refreshed.
    #[test]
    fn a_refresh_with_origin_drift_is_held() {
        use RefreshVerdict::{Act, Held};
        let drifted = Some(Held { by: HeldBy::Entry });
        let with_origin = |url: Option<&str>| ConfigFacts {
            origin_urls: url.map(OriginUrl::repo).into_iter().collect(),
            origin_keys: if url.is_some() {
                OriginKeys::InRepo
            } else {
                OriginKeys::None
            },
            origin_fetch_url: url.map(str::to_owned),
            ..ConfigFacts::default()
        };
        let lib = third_party(Mode::Head);
        for refresh in [Refresh::Named, Refresh::References] {
            for (origin, want) in [
                (Some("https://github.com/them/lib"), Some(Act)),
                (Some("https://github.com/Them/lib.git/"), Some(Act)),
                // the same repo over SSH, spelled otherwise: no drift, but
                // not the HTTPS a reference is fetched over
                (
                    Some("git@github.com:Them/lib.git"),
                    Some(Held {
                        by: HeldBy::OriginNotHttps,
                    }),
                ),
                (Some("git@github.com:me/lib"), drifted),
                (Some("https://github.com/me/lib"), drifted),
                (Some("https://github.com/them/lib2"), drifted),
                (None, drifted),
            ] {
                assert_eq!(
                    refresh_verdict(&lib, refresh, &with_origin(origin)),
                    want,
                    "{origin:?} {refresh:?}"
                );
            }
            let mut no_url = with_origin(None);
            no_url.origin_keys = OriginKeys::InRepo;
            assert_eq!(refresh_verdict(&lib, refresh, &no_url), drifted);
            assert_eq!(
                refresh_verdict(&lib, Refresh::Unasked, &with_origin(None)),
                None
            );
        }
        let fork = with_origin(Some("git@github.com:me/lib"));
        assert_eq!(
            refresh_verdict(&third_party(Mode::Pinned), Refresh::Named, &fork),
            Some(Held { by: HeldBy::Pinned })
        );
        assert_eq!(
            refresh_verdict(&owned(Mode::Follow("main")), Refresh::Named, &fork),
            None
        );
    }

    /// A refresh whose fetch wouldn't reach the registry's repo over HTTPS
    /// — its origin the repo over SSH, `http://`, or `git://`, or an
    /// `insteadOf` rewrite of its HTTPS origin, or a fetch URL never read —
    /// is held for it, with its own reason, and tracks no branch. Its fix
    /// points origin at the HTTPS URL, unless a rewrite makes the URL.
    #[test]
    fn a_refresh_not_over_https_is_held() {
        let held = Some(RefreshVerdict::Held {
            by: HeldBy::OriginNotHttps,
        });
        let lib = third_party(Mode::Head);
        let https = "https://github.com/them/lib";
        let cases: [(&str, Option<&str>, Option<OriginFix>); 6] = [
            (
                "git@github.com:them/lib",
                Some("git@github.com:them/lib"),
                Some(OriginFix::SetUrl),
            ),
            (
                "ssh://git@github.com/them/lib",
                Some("ssh://git@github.com/them/lib"),
                Some(OriginFix::SetUrl),
            ),
            (
                "http://github.com/them/lib",
                Some("http://github.com/them/lib"),
                Some(OriginFix::SetUrl),
            ),
            (
                "git://github.com/them/lib",
                Some("git://github.com/them/lib"),
                Some(OriginFix::SetUrl),
            ),
            // `url.git@github.com:.insteadOf=https://github.com/`
            (https, Some("git@github.com:them/lib"), None),
            // a rewrite to another host's HTTPS
            (https, Some("https://mirror.example/them/lib"), None),
        ];
        for (origin, fetch_url, fix) in cases {
            let mut f = lib_facts(on("main"), &[b("main", O, true, Track::Behind(3))]);
            f.config.origin_urls = vec![OriginUrl::repo(origin)];
            f.config.origin_fetch_url = fetch_url.map(str::to_owned);
            for refresh in [Refresh::Named, Refresh::References] {
                assert_eq!(refresh_verdict(&lib, refresh, &f.config), held, "{origin}");
                let c = classify(&lib, &f, &EntrySessions::idle(), refresh);
                assert!(c.branches.is_empty(), "{:?}", c.branches);
                assert_eq!(
                    c.needs_human,
                    [NeedsHuman::OriginNotHttps {
                        fetch_url: fetch_url.unwrap().to_owned(),
                        expected: https.to_owned(),
                        fix: fix.clone(),
                    }],
                    "{origin}"
                );
            }
            // unasked: nothing said of it
            let c = classify(&lib, &f, &EntrySessions::idle(), Refresh::Unasked);
            assert!(c.needs_human.is_empty(), "{:?}", c.needs_human);
        }
        // a fetch URL never read: held, failing closed
        let mut unread = lib_facts(on("main"), &[]);
        unread.config.origin_fetch_url = None;
        assert_eq!(refresh_verdict(&lib, Refresh::Named, &unread.config), held);
        // a pin named keeps its pin's refusal; origin drift says drift first
        let mut f = lib_facts(on("main"), &[]);
        f.config.origin_fetch_url = Some("git@github.com:them/lib".into());
        assert_eq!(
            refresh_verdict(&third_party(Mode::Pinned), Refresh::Named, &f.config),
            Some(RefreshVerdict::Held { by: HeldBy::Pinned })
        );
        f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/lib")];
        f.config.origin_fetch_url = Some("git@github.com:me/lib".into());
        assert_eq!(
            refresh_verdict(&lib, Refresh::Named, &f.config),
            Some(RefreshVerdict::Held { by: HeldBy::Entry })
        );
        let c = classify(&lib, &f, &EntrySessions::idle(), Refresh::Named);
        assert!(
            matches!(c.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
            "{:?}",
            c.needs_human
        );
    }

    /// An owned entry's fetch, as git resolves it, that wouldn't reach the
    /// registry's repo as sync fetches it — another repo a rewrite names,
    /// or a partial clone's over neither SSH nor HTTPS — holds the entry,
    /// with its own reason. Its fix points origin at the SSH URL, unless a
    /// rewrite makes the URL. Origin drift says drift alone, a pin is never
    /// fetched, and a fetch URL never read says nothing.
    #[test]
    fn an_owned_fetch_that_reaches_elsewhere_holds_the_entry() {
        let app = owned(Mode::Follow("main"));
        let ssh = "git@github.com:me/app";
        let branches = [b("main", O, true, Track::Ahead(1))];
        let with = |origin: &str, fetch_url: &str, partial: bool| {
            let mut f = facts(on("main"), &branches);
            f.config.origin_urls = vec![OriginUrl::repo(origin)];
            f.config.origin_fetch_url = Some(fetch_url.to_owned());
            f.config.partial_filter = partial.then(|| "blob:none".to_owned());
            f
        };
        let cases: [(&str, &str, bool, Option<OriginFix>); 5] = [
            // a rewrite to another repo
            (ssh, "git@github.com:me/other", false, None),
            (ssh, "file:///srv/app.git", false, None),
            // a partial clone over a transport its lazy fetch may not take
            (ssh, "git://github.com/me/app", true, None),
            (
                "http://github.com/me/app",
                "http://github.com/me/app",
                true,
                Some(OriginFix::SetUrl),
            ),
            (
                "git://github.com/me/app",
                "git://github.com/me/app",
                true,
                Some(OriginFix::SetUrl),
            ),
        ];
        for (origin, fetch_url, partial, fix) in cases {
            let f = with(origin, fetch_url, partial);
            assert_eq!(
                fetch_url_mismatch(&app, &f.config),
                Some(fetch_url),
                "{fetch_url}"
            );
            let c = classify(&app, &f, &EntrySessions::idle(), Refresh::Unasked);
            assert_eq!(
                c.needs_human,
                [NeedsHuman::FetchUrlMismatch {
                    fetch_url: fetch_url.to_owned(),
                    expected: ssh.to_owned(),
                    fix,
                }],
                "{fetch_url}"
            );
            assert!(c.needs_human[0].holds_entry());
            assert_eq!(
                c.branches[0].verdict,
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::Entry
                },
                "{fetch_url}"
            );
        }
        // the registry's repo, however git reaches it: nothing said
        for (fetch_url, partial) in [
            (ssh, true),
            ("https://github.com/me/app", true),
            ("ssh://git@github.com/me/app.git", true),
            // a whole clone fetches over whatever names the repo
            ("git://github.com/me/app", false),
        ] {
            let f = with(ssh, fetch_url, partial);
            assert_eq!(fetch_url_mismatch(&app, &f.config), None, "{fetch_url}");
            let c = classify(&app, &f, &EntrySessions::idle(), Refresh::Unasked);
            assert!(c.needs_human.is_empty(), "{fetch_url}: {:?}", c.needs_human);
        }
        // never read: nothing said (the probe doesn't fetch it)
        let unread = facts(on("main"), &branches);
        assert_eq!(fetch_url_mismatch(&app, &unread.config), None);
        // a pin, and a reference: never this reason
        let other = with(ssh, "git@github.com:me/other", false);
        assert_eq!(
            fetch_url_mismatch(&owned(Mode::Pinned), &other.config),
            None
        );
        assert_eq!(
            fetch_url_mismatch(&third_party(Mode::Head), &other.config),
            None
        );
        // origin drift says drift alone
        let drifted = with("git@github.com:me/other", "git@github.com:me/other", false);
        let c = classify(&app, &drifted, &EntrySessions::idle(), Refresh::Unasked);
        assert!(
            matches!(c.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
            "{:?}",
            c.needs_human
        );
    }

    /// A drifted refresh compares no branch against origin: only local
    /// work is said, as for a reference no run asks about, and the drift
    /// is the entry's reason.
    #[test]
    fn a_drifted_refresh_tracks_no_branch() {
        let mut f = lib_facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(3)),
                b("audit", None, false, Track::Even).unique(1),
            ],
        );
        f.config.origin_urls = vec![OriginUrl::repo("git@github.com:me/lib")];
        let c = classify(
            &third_party(Mode::Head),
            &f,
            &EntrySessions::idle(),
            Refresh::Named,
        );
        let names: Vec<(&str, Relation)> = c
            .branches
            .iter()
            .map(|b| (b.name.as_str(), b.relation))
            .collect();
        assert_eq!(names, [("audit", Relation::Untracked)]);
        assert!(matches!(
            c.needs_human[..],
            [NeedsHuman::OriginMismatch { .. }]
        ));
    }

    /// A third-party reference's facts, its origin the registry's URL.
    fn lib_facts(head: Head, branches: &[B<'_>]) -> RepoFacts {
        let mut f = facts(head, branches);
        f.config.origin_urls = vec![OriginUrl::repo("https://github.com/them/lib")];
        f.config.origin_fetch_url = Some("https://github.com/them/lib".into());
        // never read for a third-party reference
        f.push_urls = None;
        f
    }

    #[test]
    fn a_refreshed_reference_is_compared_against_origin_and_never_pushed() {
        let mut f = lib_facts(
            on("main"),
            &[
                b("main", O, true, Track::Behind(3)),
                b("feat", O, true, Track::Behind(1)),
                // commits on no remote: local work
                b("audit", O, true, Track::Ahead(2)).unique(2),
                // ahead of origin, its commits on another remote already
                b("mirror", O, true, Track::Ahead(1)),
                b(
                    "arc",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 1,
                    },
                )
                .unique(1),
                b("even", O, true, Track::Even),
            ],
        );
        let lib = third_party(Mode::Head);
        let refreshed = |f: &RepoFacts, refresh| classify(&lib, f, &EntrySessions::idle(), refresh);
        for refresh in [Refresh::Named, Refresh::References] {
            let c = refreshed(&f, refresh);
            assert!(c.needs_human.is_empty(), "{:?}", c.needs_human);
            let got: Vec<_> = c
                .branches
                .into_iter()
                .map(|b| (b.name, b.verdict))
                .collect();
            assert_eq!(
                got,
                named(&[
                    ("main", act(SyncAction::FastForward { commits: 3 })),
                    ("feat", act(SyncAction::FastForward { commits: 1 })),
                    ("audit", Verdict::LocalOnly),
                    ("mirror", Verdict::Quiet),
                    ("arc", needs(BranchNeedsHuman::Diverged)),
                    ("even", Verdict::Quiet),
                ]),
                "{refresh:?}"
            );
        }
        // a dirty checkout holds the branch it's on, not the others
        f.status.uncommitted.untracked = 1;
        let c = refreshed(&f, Refresh::Named);
        assert_eq!(
            c.branches[0].verdict,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 3 },
                by: HeldBy::DirtyCheckout,
            }
        );
        assert_eq!(
            c.branches[1].verdict,
            act(SyncAction::FastForward { commits: 1 })
        );
        // unasked: local work alone, compared against nothing
        f.status.uncommitted.untracked = 0;
        let c = refreshed(&f, Refresh::Unasked);
        let got: Vec<_> = c
            .branches
            .into_iter()
            .map(|b| (b.name, b.relation, b.verdict))
            .collect();
        assert_eq!(
            got,
            [
                ("audit".to_owned(), Relation::Untracked, Verdict::LocalOnly),
                ("arc".to_owned(), Relation::Untracked, Verdict::LocalOnly),
            ]
        );
        // a pin named keeps its pin's verdicts
        let pinned = third_party(Mode::Pinned);
        assert_eq!(
            verdicts(&pinned, &f),
            classify(&pinned, &f, &EntrySessions::idle(), Refresh::Named)
                .branches
                .into_iter()
                .map(|b| (b.name, b.verdict))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_refreshed_shallow_reference_moves_when_nothing_local_is_at_stake() {
        let mut f = lib_facts(
            on("main"),
            &[
                b(
                    "main",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 1,
                    },
                ),
                b("tip", O, true, Track::Ahead(1)).unique(1).on_tip(),
                b(
                    "stranded",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 2,
                        behind: 1,
                    },
                )
                .unique(1),
            ],
        );
        f.layout.shallow = true;
        let c = classify(
            &third_party(Mode::Head),
            &f,
            &EntrySessions::idle(),
            Refresh::Named,
        );
        let got: Vec<_> = c
            .branches
            .into_iter()
            .map(|b| (b.name, b.verdict))
            .collect();
        assert_eq!(
            got,
            named(&[
                ("main", act(SyncAction::Move)),
                // on the fetched tip, but never pushed
                ("tip", Verdict::LocalOnly),
                ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
            ])
        );
    }

    #[test]
    fn a_missing_entry_cloned_under_another_name_is_held() {
        let stray = |dir: &str, origin: Option<&str>, kind| UnregisteredClone {
            dir: dir.into(),
            origin: origin.map(str::to_owned),
            owned: true,
            kind,
        };
        let e = owned(Mode::Follow("main"));
        let recipe = clone_recipe(&e);
        let held = |dirs: &[&str]| ClassifiedMissing {
            clone: CloneVerdict::Held {
                recipe: recipe.clone(),
                by: HeldBy::Entry,
            },
            needs_human: dirs
                .iter()
                .map(|d| NeedsHuman::ClonedUnregistered {
                    dir: (*d).to_owned(),
                })
                .collect(),
        };
        // its repo by any of git's spellings, a credential redacted
        for origin in [
            "git@github.com:me/app",
            "ssh://git@github.com/me/app.git",
            "https://github.com/Me/App/",
            "https://***@github.com/me/app",
        ] {
            let found = [stray("app-old", Some(origin), UnregisteredKind::Clone)];
            assert_eq!(
                classify_missing(&e, false, false, &found),
                held(&["app-old"]),
                "{origin}"
            );
        }
        // each dir that clones it, a worktree of an unregistered clone too,
        // named before a session at the path
        let found = [
            stray("a", Some("git@github.com:me/app"), UnregisteredKind::Clone),
            stray(
                "b",
                Some("git@github.com:me/app"),
                UnregisteredKind::Worktree,
            ),
        ];
        assert_eq!(classify_missing(&e, true, false, &found), held(&["a", "b"]));
        // another repo, no origin, or the tool's own temp dir: nothing
        let unrelated = [
            stray("x", Some("git@github.com:me/apps"), UnregisteredKind::Clone),
            stray("y", Some("git@evil.com:me/app"), UnregisteredKind::Clone),
            stray("z", None, UnregisteredKind::Clone),
            stray(
                ".app.repos-clone-1-0123456789abcdef",
                Some("git@github.com:me/app"),
                UnregisteredKind::UnfinishedClone,
            ),
        ];
        assert_eq!(
            classify_missing(&e, false, false, &unrelated),
            ClassifiedMissing {
                clone: CloneVerdict::Act { recipe },
                needs_human: vec![],
            }
        );
        assert!(NeedsHuman::ClonedUnregistered { dir: "a".into() }.holds_entry());
    }

    /// A rename differing only in ASCII case and `-` against `_` holds the
    /// clone; any other name, account, or host doesn't — nor is it origin
    /// drift's or a push URL's match, which stay exact.
    #[test]
    fn a_missing_entry_cloned_under_its_renamed_name_is_held() {
        let e = Entry {
            url: url("https://github.com/me/vscode-extension-tsv-format"),
            ..owned(Mode::Follow("main"))
        };
        let stray = |origin: &str| UnregisteredClone {
            dir: "old".into(),
            origin: Some(origin.to_owned()),
            owned: true,
            kind: UnregisteredKind::Clone,
        };
        for origin in [
            "git@github.com:me/vscode_extension_tsv_format",
            "https://github.com/ME/VSCode_Extension-TSV_Format.git",
            "git@github.com:me/vscode-extension-tsv-format",
        ] {
            let c = classify_missing(&e, false, false, &[stray(origin)]);
            assert_eq!(
                c.needs_human,
                [NeedsHuman::ClonedUnregistered { dir: "old".into() }],
                "{origin}"
            );
            assert!(
                matches!(
                    c.clone,
                    CloneVerdict::Held {
                        by: HeldBy::Entry,
                        ..
                    }
                ),
                "{origin}"
            );
        }
        for origin in [
            "git@github.com:me/vscode.extension.tsv.format",
            "git@github.com:me/vscode-extension-tsv-formats",
            "git@github.com:me/vscodeextensiontsvformat",
            "git@github.com:you/vscode_extension_tsv_format",
            "git@gitlab.com:me/vscode_extension_tsv_format",
            "ssh://git@github.com:22/me/vscode_extension_tsv_format",
        ] {
            let c = classify_missing(&e, false, false, &[stray(origin)]);
            assert!(c.needs_human.is_empty(), "{origin}: {:?}", c.needs_human);
            assert!(matches!(c.clone, CloneVerdict::Act { .. }), "{origin}");
        }
        let renamed = "git@github.com:me/vscode_extension_tsv_format";
        assert!(!origin_matches(renamed, &e.url));
        assert!(!push_urls_match(&[renamed.to_owned()], &e.url));
    }

    #[test]
    fn a_missing_entry_is_cloned_by_its_recipe() {
        let act = |recipe| CloneVerdict::Act { recipe };
        // owned: over SSH, on its branch
        let owned_recipe = CloneRecipe {
            url: "git@github.com:me/app".into(),
            branch: Some("main".into()),
            shallow: false,
            sparse: None,
        };
        assert_eq!(
            classify_missing(&owned(Mode::Follow("main")), false, false, &[]).clone,
            act(owned_recipe.clone())
        );
        // third-party: over HTTPS, shallow and sparse as declared, the
        // remote's default branch without one
        let lib = Entry {
            shallow: true,
            sparse: Some("css".into()),
            ..third_party(Mode::Head)
        };
        assert_eq!(
            classify_missing(&lib, false, false, &[]).clone,
            act(CloneRecipe {
                url: "https://github.com/them/lib".into(),
                branch: None,
                shallow: true,
                sparse: Some("css".into()),
            })
        );
        // a pin and an archived repo are cloned all the same
        for e in [
            owned(Mode::PinnedOn("fork")),
            Entry {
                archived: true,
                ..owned(Mode::Follow("main"))
            },
        ] {
            assert!(
                matches!(
                    classify_missing(&e, false, false, &[]).clone,
                    CloneVerdict::Act { .. }
                ),
                "{e:?}"
            );
        }
        // a session at the path holds it, named before a recorded worktree
        let e = owned(Mode::Follow("main"));
        for (busy, recorded, by) in [
            (true, false, HeldBy::Busy),
            (true, true, HeldBy::Busy),
            (false, true, HeldBy::UnprobedWorktree),
        ] {
            assert_eq!(
                classify_missing(&e, busy, recorded, &[]),
                ClassifiedMissing {
                    clone: CloneVerdict::Held {
                        recipe: owned_recipe.clone(),
                        by
                    },
                    needs_human: vec![],
                }
            );
        }
        // another entry naming its repo holds it for a person, named before
        // anything else
        let shared = Entry {
            same_repo_as: Some("app_wt".into()),
            ..owned(Mode::Follow("main"))
        };
        for (busy, recorded) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                classify_missing(&shared, busy, recorded, &[]),
                ClassifiedMissing {
                    clone: CloneVerdict::Held {
                        recipe: owned_recipe.clone(),
                        by: HeldBy::Entry
                    },
                    needs_human: vec![NeedsHuman::CloneSharesRepo {
                        with: "app_wt".into()
                    }],
                },
                "{busy} {recorded}"
            );
        }
        assert!(
            NeedsHuman::CloneSharesRepo {
                with: "app_wt".into()
            }
            .holds_entry()
        );
    }

    #[test]
    fn archived_and_pinned_verdicts() {
        let f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Ahead(1)).unique(1),
                b("feat", O, true, Track::Behind(2)),
            ],
        );
        let archived = Entry {
            archived: true,
            ..owned(Mode::Follow("main"))
        };
        assert_eq!(
            verdicts(&archived, &f),
            named(&[
                ("main", needs(BranchNeedsHuman::ArchivedAhead)),
                // the host serves reads, so behind still fast-forwards
                ("feat", act(SyncAction::FastForward { commits: 2 })),
            ])
        );
        // a pin is never fetched, so its commits are local work, and every
        // other action is the pin's to hold
        assert_eq!(
            verdicts(&owned(Mode::Pinned), &f),
            named(&[
                ("main", Verdict::LocalOnly),
                (
                    "feat",
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 2 },
                        by: HeldBy::Pinned,
                    },
                ),
            ])
        );
    }

    #[test]
    fn shallow_verdicts() {
        let mut f = facts(
            on("main"),
            &[
                b(
                    "moved",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 1,
                    },
                ),
                b(
                    "stranded",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 2,
                        behind: 1,
                    },
                )
                .unique(1),
            ],
        );
        f.layout.shallow = true;
        assert_eq!(
            verdicts(&owned(Mode::Head), &f),
            named(&[
                ("moved", act(SyncAction::Move)),
                ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
            ])
        );
    }

    #[test]
    fn third_party_local_work_is_local_only() {
        let f = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("audit", None, false, Track::Even).unique(2)],
        );
        assert_eq!(
            verdicts(&third_party(Mode::Head), &f),
            named(&[("audit", Verdict::LocalOnly)])
        );
    }

    #[test]
    fn shallow_relations_never_count_across_roots() {
        let mut f = facts(
            on("main"),
            &[
                // tips match
                b("main", O, true, Track::Even),
                // origin moved: git says ahead 1 behind 1, the root subtracted
                b(
                    "moved",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 1,
                        behind: 1,
                    },
                ),
                // a local commit on the fetched tip
                b("on-tip", O, true, Track::Ahead(1)).unique(1).on_tip(),
                // a local commit on the old root, origin moved
                b(
                    "stranded",
                    O,
                    true,
                    Track::Diverged {
                        ahead: 2,
                        behind: 1,
                    },
                )
                .unique(1),
                b("gone", O, true, Track::Gone),
            ],
        );
        f.layout.shallow = true;
        let got = relations(&owned(Mode::Follow("main")), &f);
        assert_eq!(
            got.into_iter().map(|(_, r)| r).collect::<Vec<_>>(),
            [
                Relation::InSync,
                Relation::Shallow,
                Relation::Ahead { commits: 1 },
                Relation::Shallow,
                Relation::Gone,
            ]
        );
    }

    #[test]
    fn third_party_keeps_only_local_work() {
        let f = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[
                b("main", O, true, Track::Behind(57)),
                b("tsv-format-audit", None, false, Track::Even).unique(3),
                b("master", O, true, Track::Gone),
            ],
        );
        let c = classify(
            &third_party(Mode::Pinned),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        );
        assert_eq!(c.branches.len(), 1);
        assert_eq!(c.branches[0].name, "tsv-format-audit");
        assert_eq!(c.branches[0].relation, Relation::Untracked);
        assert_eq!(c.branches[0].unique_commits, 3);
        assert_eq!(c.branches[0].newest_commit_at, NOW - 3600);
        // origin is the registry url in SSH form for the owned fixture; the
        // third-party url differs
        assert!(matches!(
            c.needs_human[..],
            [NeedsHuman::OriginMismatch { .. }]
        ));
    }

    #[test]
    fn follow_mode_reasons() {
        let e = owned(Mode::Follow("main"));
        let missing = facts(on("dev"), &[b("dev", O, true, Track::Even)]);
        assert_eq!(
            classify(&e, &missing, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            [NeedsHuman::DefaultBranchMissing {
                branch: "main".into()
            }]
        );
        let no_upstream = facts(on("main"), &[b("main", None, false, Track::Even)]);
        assert_eq!(
            classify(&e, &no_upstream, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        let other_remote = facts(
            on("main"),
            &[b("main", Some("upstream"), true, Track::Even)],
        );
        assert_eq!(
            classify(&e, &other_remote, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            [NeedsHuman::DefaultBranchNoUpstream {
                branch: "main".into()
            }]
        );
        // unmapped is a branch-level reason, not a missing upstream
        let unmapped = facts(on("main"), &[b("main", O, false, Track::Even)]);
        assert!(
            classify(&e, &unmapped, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
        let detached = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("main", O, true, Track::Even)],
        );
        assert_eq!(
            classify(&e, &detached, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            [NeedsHuman::UnexpectedDetached {
                checkout: "/ws/app".into()
            }]
        );
        // on a feature branch is not a finding
        let feature = facts(
            on("feat"),
            &[
                b("main", O, true, Track::Even),
                b("feat", O, true, Track::Even),
            ],
        );
        assert!(
            classify(&e, &feature, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
    }

    /// The branch an entry follows, its upstream gone from origin (the
    /// remote's default renamed), needs a person — never cleanup, never a
    /// worktree to remove — and the branch itself reads as one with no
    /// upstream does. Any other gone branch stays cleanup.
    #[test]
    fn a_followed_branch_whose_upstream_is_gone_needs_a_human() {
        let e = owned(Mode::Follow("master"));
        let reason = [NeedsHuman::DefaultBranchGone {
            branch: "master".into(),
        }];
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("master", O, true, Track::Gone),
                b("old", O, true, Track::Gone),
            ],
        );
        // the followed branch in a clean linked worktree, as removable as
        // `old` would be
        f.worktrees = vec![linked("/ws/app-master", on("master"))];
        let c = classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked);
        assert_eq!(c.needs_human, reason);
        assert!(!reason[0].holds_entry());
        assert_eq!(
            verdicts(&e, &f),
            named(&[
                ("main", Verdict::Quiet),
                ("master", Verdict::Quiet),
                (
                    "old",
                    Verdict::Cleanup {
                        reason: CleanupReason::UpstreamGone,
                        removable_worktree: None,
                    }
                ),
            ])
        );
        assert_eq!(branch_relation(&c, "master"), Relation::Gone);
        // with commits on no remote, it's local work, never deletable
        let unique = facts(on("master"), &[b("master", O, true, Track::Gone).unique(2)]);
        assert_eq!(
            classify(&e, &unique, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            reason
        );
        assert_eq!(
            verdicts(&e, &unique),
            named(&[("master", Verdict::LocalOnly)])
        );
        // following another branch, it's cleanup as ever
        let main = owned(Mode::Follow("main"));
        let gone = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("master", O, true, Track::Gone),
            ],
        );
        assert!(
            classify(&main, &gone, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
        assert!(matches!(
            verdicts(&main, &gone)[1].1,
            Verdict::Cleanup { .. }
        ));
        // a pin's refs are stale by contract, and a reference the run
        // doesn't refresh is compared against no remote: neither says it
        for e in [
            owned(Mode::PinnedOn("master")),
            third_party(Mode::Follow("master")),
        ] {
            let c = classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked);
            assert!(
                !c.needs_human
                    .iter()
                    .any(|r| matches!(r, NeedsHuman::DefaultBranchGone { .. })),
                "{:?}",
                c.needs_human
            );
        }
        // a reference the run refreshes does
        let mut refreshed = f;
        refreshed.config.origin_urls = vec![OriginUrl::repo("https://github.com/them/lib")];
        refreshed.config.origin_fetch_url = Some("https://github.com/them/lib".into());
        refreshed.push_urls = None;
        let lib = third_party(Mode::Follow("master"));
        assert_eq!(
            classify(&lib, &refreshed, &EntrySessions::idle(), Refresh::Named).needs_human,
            reason
        );
    }

    /// With `worktrees/` itself unreadable, or an unprobed worktree whose
    /// HEAD is unknown, any branch may be checked out where no one can
    /// see: none is cleanup. A gone worktree whose HEAD reads doesn't
    /// withhold it.
    #[test]
    fn an_unreadable_worktrees_dir_withholds_cleanup() {
        let e = owned(Mode::Follow("main"));
        let mut f = facts(
            on("main"),
            &[
                b("main", O, true, Track::Even),
                b("gone", O, true, Track::Gone),
                b("gone-work", O, true, Track::Gone).unique(1),
                b("merged", None, false, Track::Even),
            ],
        );
        f.unreadable = vec!["/ws/app/.git/worktrees".into()];
        assert_eq!(
            verdicts(&e, &f),
            named(&[
                ("main", Verdict::Quiet),
                ("gone", Verdict::Quiet),
                ("gone-work", Verdict::LocalOnly),
                ("merged", Verdict::Quiet),
            ])
        );
        let withheld = verdicts(&e, &f);
        // one admin dir unreadable: its worktree's HEAD is unknown
        f.unreadable = vec!["/ws/app/.git/worktrees/x".into()];
        f.unprobed = vec![UnprobedWorktree {
            head: UnprobedHead::Unknown,
            ..unprobed(
                "/ws/app/.git/worktrees/x",
                None,
                UnprobedWhy::Failed {
                    error: "not listed by git".into(),
                },
            )
        }];
        assert_eq!(verdicts(&e, &f), withheld);
        // a readable admin dir whose HEAD isn't
        f.unreadable.clear();
        assert_eq!(verdicts(&e, &f), withheld);
        // a gone worktree on a known branch: cleanup as ever
        f.unprobed = vec![unprobed("/ws/app-x", Some("main"), UnprobedWhy::Prunable)];
        let cleanup = verdicts(&e, &f)
            .into_iter()
            .filter(|(_, v)| matches!(v, Verdict::Cleanup { .. }))
            .count();
        assert_eq!(cleanup, 3);
    }

    fn branch_relation(c: &Classified, name: &str) -> Relation {
        c.branches.iter().find(|b| b.name == name).unwrap().relation
    }

    #[test]
    fn pinned_and_head_modes() {
        let detached = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("main", O, true, Track::Behind(1))],
        );
        let on_main = facts(on("main"), &[b("main", O, true, Track::Even)]);
        let pinned = owned(Mode::Pinned);
        let head = owned(Mode::Head);
        // a pin's HEAD is its consumer's: detached or on a branch, nothing
        // to say
        for f in [&detached, &on_main] {
            assert!(
                classify(&pinned, f, &EntrySessions::idle(), Refresh::Unasked)
                    .needs_human
                    .is_empty()
            );
        }
        assert!(
            classify(&head, &detached, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
        assert!(
            classify(&head, &on_main, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );
    }

    #[test]
    fn a_pin_on_its_branch_expects_nothing_of_it() {
        let pinned = owned(Mode::PinnedOn("fork"));
        let reasons = |f: &RepoFacts| {
            classify(&pinned, f, &EntrySessions::idle(), Refresh::Unasked).needs_human
        };
        // on its branch, behind a stale remote-tracking ref: held, not
        // reported
        let on_fork = facts(on("fork"), &[b("fork", O, true, Track::Behind(3))]);
        assert!(reasons(&on_fork).is_empty());
        assert_eq!(
            verdicts(&pinned, &on_fork),
            named(&[(
                "fork",
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 3 },
                    by: HeldBy::Pinned,
                },
            )])
        );
        // detached, on another branch, its branch missing or with no
        // upstream: none of the follow reasons
        let detached = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("fork", O, true, Track::Even)],
        );
        let elsewhere = facts(on("main"), &[b("main", O, true, Track::Even)]);
        let no_upstream = facts(on("fork"), &[b("fork", None, false, Track::Even)]);
        for f in [&detached, &elsewhere, &no_upstream] {
            assert!(reasons(f).is_empty());
        }
        // the same branch, followed rather than pinned, has all three
        let followed = owned(Mode::Follow("fork"));
        let followed_reasons = |f: &RepoFacts| {
            classify(&followed, f, &EntrySessions::idle(), Refresh::Unasked).needs_human
        };
        assert!(matches!(
            followed_reasons(&detached)[..],
            [NeedsHuman::UnexpectedDetached { .. }]
        ));
        assert!(matches!(
            followed_reasons(&elsewhere)[..],
            [NeedsHuman::DefaultBranchMissing { .. }]
        ));
        assert!(matches!(
            followed_reasons(&no_upstream)[..],
            [NeedsHuman::DefaultBranchNoUpstream { .. }]
        ));
        // its branch with nothing unique and no upstream isn't merged
        // cleanup: it's where the pin lives
        let detached_bare = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("fork", None, false, Track::Even)],
        );
        assert_eq!(
            verdicts(&pinned, &detached_bare),
            named(&[("fork", Verdict::Quiet)])
        );
    }

    #[test]
    fn a_stale_main_beside_a_pin_is_held_by_it() {
        let held = |commits| Verdict::Held {
            action: SyncAction::FastForward { commits },
            by: HeldBy::Pinned,
        };
        // detached at the pin, and on the pin's branch: a local main far
        // behind a stale origin/main never moves, and local work stays
        // local
        for head in [
            Head::Detached {
                commit: "abc".into(),
            },
            on("fork"),
        ] {
            let f = facts(
                head,
                &[
                    b("fork", O, true, Track::Even),
                    b("main", O, true, Track::Behind(57)),
                    b("audit", None, false, Track::Even).unique(2),
                    b("ahead", O, true, Track::Ahead(1)).unique(1),
                ],
            );
            let c = classify(
                &owned(Mode::PinnedOn("fork")),
                &f,
                &EntrySessions::idle(),
                Refresh::Unasked,
            );
            assert!(c.needs_human.is_empty());
            assert_eq!(
                c.branches
                    .iter()
                    .map(|b| (b.name.clone(), b.verdict.clone()))
                    .collect::<Vec<_>>(),
                named(&[
                    ("fork", Verdict::Quiet),
                    ("main", held(57)),
                    ("audit", Verdict::LocalOnly),
                    ("ahead", Verdict::LocalOnly),
                ])
            );
        }
    }

    #[test]
    fn a_pin_branch_ahead_reads_its_unique_commits() {
        // a fork's main fast-forwarded from upstream: ahead of a stale
        // origin/main with every commit on a remote ref, so nothing local
        let f = facts(
            on("fork"),
            &[
                b("fork", O, true, Track::Even),
                b("main", O, true, Track::Ahead(2)),
                b("work", O, true, Track::Ahead(2)).unique(1),
            ],
        );
        assert_eq!(
            verdicts(&owned(Mode::PinnedOn("fork")), &f),
            named(&[
                ("fork", Verdict::Quiet),
                ("main", Verdict::Quiet),
                ("work", Verdict::LocalOnly),
            ])
        );
    }

    #[test]
    fn a_pin_gets_no_verdict_from_its_stale_refs() {
        let pinned = owned(Mode::PinnedOn("fork"));
        let diverged = Track::Diverged {
            ahead: 1,
            behind: 1,
        };
        let f = facts(
            on("fork"),
            &[
                b("fork", O, true, Track::Gone).unique(1),
                b("side", O, true, Track::Gone),
                b("diverged", O, true, diverged).unique(1),
                b("unmapped", O, false, Track::Even).unique(5),
                b("merged", None, false, Track::Even),
                b("ahead", O, true, Track::Ahead(2)).unique(2),
                b("behind", O, true, Track::Behind(3)),
            ],
        );
        let held = |action| Verdict::Held {
            action,
            by: HeldBy::Pinned,
        };
        // never fetched, so no ref's word is taken: local work where the
        // branch has commits on no remote, else nothing — not the gone
        // branch the pin lives on to clean up, nor a merged one
        assert_eq!(
            verdicts(&pinned, &f),
            named(&[
                ("fork", Verdict::LocalOnly),
                ("side", Verdict::Quiet),
                ("diverged", Verdict::LocalOnly),
                ("unmapped", Verdict::LocalOnly),
                ("merged", Verdict::Quiet),
                ("ahead", Verdict::LocalOnly),
                ("behind", held(SyncAction::FastForward { commits: 3 })),
            ])
        );
        // followed, the same refs are taken at their word — the followed
        // branch's gone upstream its entry's reason, never cleanup
        let gone = |removable_worktree| Verdict::Cleanup {
            reason: CleanupReason::UpstreamGone,
            removable_worktree,
        };
        assert_eq!(
            verdicts(&owned(Mode::Follow("fork")), &f),
            named(&[
                ("fork", Verdict::LocalOnly),
                ("side", gone(None)),
                ("diverged", needs(BranchNeedsHuman::Diverged)),
                ("unmapped", needs(BranchNeedsHuman::Unmapped)),
                (
                    "merged",
                    Verdict::Cleanup {
                        reason: CleanupReason::Merged,
                        removable_worktree: None,
                    },
                ),
                ("ahead", act(SyncAction::Push { commits: 2 })),
                ("behind", act(SyncAction::FastForward { commits: 3 })),
            ])
        );
        // shallow: the stranded work is local, the stale pointer held
        let mut f = facts(
            on("fork"),
            &[
                b("moved", O, true, diverged),
                b("stranded", O, true, diverged).unique(1),
            ],
        );
        f.layout.shallow = true;
        assert_eq!(
            verdicts(&pinned, &f),
            named(&[
                ("moved", held(SyncAction::Move)),
                ("stranded", Verdict::LocalOnly),
            ])
        );
        assert_eq!(
            verdicts(&owned(Mode::Follow("fork")), &f),
            named(&[
                ("moved", act(SyncAction::Move)),
                ("stranded", needs(BranchNeedsHuman::ShallowLocalWork)),
            ])
        );
    }

    #[test]
    fn a_pin_names_the_hold_before_busy_dirt_and_the_entry() {
        let ff = SyncAction::FastForward { commits: 2 };
        let mut f = facts(
            on("fork"),
            &[
                b("fork", O, true, Track::Behind(2)),
                b("local", O, true, Track::Ahead(1)).unique(1),
            ],
        );
        f.status.uncommitted.unstaged = 1;
        let pinned = owned(Mode::PinnedOn("fork"));
        let followed = owned(Mode::Follow("fork"));
        let busy = busy_at(&["/ws/app"]);
        // followed: the busy dirty checkout names the hold, and holds the
        // push
        assert_eq!(
            verdicts_with(&followed, &f, &busy),
            [
                Verdict::Held {
                    action: ff,
                    by: HeldBy::Busy
                },
                act(SyncAction::Push { commits: 1 }),
            ]
        );
        // pinned: the pin, which clearing the session or the dirt never
        // releases; its commits stay local work
        let pinned_verdicts = [
            Verdict::Held {
                action: ff,
                by: HeldBy::Pinned,
            },
            Verdict::LocalOnly,
        ];
        assert_eq!(verdicts_with(&pinned, &f, &busy), pinned_verdicts);
        assert_eq!(
            verdicts_with(&pinned, &f, &EntrySessions::idle()),
            pinned_verdicts
        );
        // busy detection unavailable too
        assert_eq!(
            verdicts_with(&pinned, &f, &EntrySessions::unavailable()),
            pinned_verdicts
        );
        // an entry-level reason stays on the entry, the pin still named
        f.in_progress = Some(InProgressOp::Merge);
        let c = classify(&pinned, &f, &busy, Refresh::Unasked);
        assert!(matches!(
            c.needs_human[..],
            [NeedsHuman::OperationInProgress { .. }]
        ));
        assert_eq!(
            c.branches
                .into_iter()
                .map(|b| b.verdict)
                .collect::<Vec<_>>(),
            pinned_verdicts
        );
    }

    #[test]
    fn in_progress_and_origin_reasons() {
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        f.in_progress = Some(InProgressOp::Rebase);
        f.config.origin_urls.clear();
        f.config.origin_keys = OriginKeys::None;
        assert_eq!(
            classify(
                &owned(Mode::Follow("main")),
                &f,
                &EntrySessions::idle(),
                Refresh::Unasked
            )
            .needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op: InProgressOp::Rebase
                },
                NeedsHuman::OriginMismatch {
                    origin: OriginRemote::Missing,
                    expected: "git@github.com:me/app".into(),
                    fix: OriginFix::Add,
                },
            ]
        );
        // an `origin` with keys but no URL, the repo's own
        f.config.origin_keys = OriginKeys::InRepo;
        assert!(
            classify(
                &owned(Mode::Follow("main")),
                &f,
                &EntrySessions::idle(),
                Refresh::Unasked
            )
            .needs_human
            .contains(&NeedsHuman::OriginMismatch {
                origin: OriginRemote::NoUrl,
                expected: "git@github.com:me/app".into(),
                fix: OriginFix::SetUrl,
            })
        );
        // `origin` only in global config: no `git remote` command can edit it
        // there, and `remote add` would add a URL after it
        f.config.origin_keys = OriginKeys::Elsewhere;
        f.config.origin_urls = vec![OriginUrl::elsewhere("git@github.com:old/app")];
        let reason = |f: &RepoFacts| {
            classify(
                &owned(Mode::Follow("main")),
                f,
                &EntrySessions::idle(),
                Refresh::Unasked,
            )
            .needs_human
            .into_iter()
            .find(|r| matches!(r, NeedsHuman::OriginMismatch { .. }))
        };
        assert_eq!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                origin: OriginRemote::Url {
                    url: "git@github.com:old/app".into()
                },
                expected: "git@github.com:me/app".into(),
                fix: OriginFix::ByHand {
                    reason: OriginByHand::OutsideRepoFile
                },
            })
        );
        // `origin` known only through a global fetch refspec: `remote add`
        f.config.origin_urls.clear();
        assert!(matches!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                origin: OriginRemote::NoUrl,
                fix: OriginFix::Add,
                ..
            })
        ));
    }

    #[test]
    fn origin_urls_as_git_reads_them() {
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        let reason = |f: &RepoFacts| {
            classify(
                &owned(Mode::Follow("main")),
                f,
                &EntrySessions::idle(),
                Refresh::Unasked,
            )
            .needs_human
            .into_iter()
            .find(|r| matches!(r, NeedsHuman::OriginMismatch { .. }))
        };
        // read where git connects, never by the text: a lookalike is drift
        for lookalike in LOOKALIKE_URLS {
            f.config.origin_urls = vec![OriginUrl::repo(lookalike)];
            assert!(reason(&f).is_some(), "{lookalike}");
        }
        // an uppercase host is still the registry's
        f.config.origin_urls = vec![OriginUrl::repo("git@GITHUB.com:me/app")];
        assert_eq!(reason(&f), None);
        // the first URL wins: a mismatch first, the registry's second, is
        // still a mismatch — and the other way round isn't
        f.config.origin_urls = vec![
            OriginUrl::repo("https://me:ghp_TOKEN@github.com/old/app"),
            OriginUrl::repo("git@github.com:me/app"),
        ];
        assert_eq!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                // the credential never reaches the report
                origin: OriginRemote::Url {
                    url: "https://***@github.com/old/app".into()
                },
                expected: "git@github.com:me/app".into(),
                // several URLs: no command fits every shape of them
                fix: OriginFix::ByHand {
                    reason: OriginByHand::SeveralUrls
                },
            })
        );
        f.config.origin_urls.reverse();
        assert_eq!(reason(&f), None);
        // an empty value resets the list: nothing left is no URL, and the
        // reset is beyond what `set-url` can reason about
        f.config.origin_urls = vec![
            OriginUrl::repo("git@github.com:me/app"),
            OriginUrl::repo(""),
        ];
        assert_eq!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                origin: OriginRemote::NoUrl,
                expected: "git@github.com:me/app".into(),
                fix: OriginFix::ByHand {
                    reason: OriginByHand::EmptyValue
                },
            })
        );
        // a reset then a mismatch: that one is what git fetches from
        f.config
            .origin_urls
            .push(OriginUrl::repo("git@github.com:old/app"));
        assert!(matches!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                origin: OriginRemote::Url { .. },
                fix: OriginFix::ByHand {
                    reason: OriginByHand::EmptyValue
                },
                ..
            })
        ));
        // a single empty value: `set-url` replaces it; a valueless one
        // breaks every git remote command
        f.config.origin_urls = vec![OriginUrl::repo("")];
        assert_eq!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                origin: OriginRemote::NoUrl,
                expected: "git@github.com:me/app".into(),
                fix: OriginFix::SetUrl,
            })
        );
        f.config.origin_urls = vec![OriginUrl::valueless()];
        assert!(matches!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                fix: OriginFix::ByHand {
                    reason: OriginByHand::ValuelessUrl
                },
                ..
            })
        ));
        // one URL in the repo's file: a plain `set-url`
        f.config.origin_urls = vec![OriginUrl::repo("git@github.com:old/app.git")];
        assert!(matches!(
            reason(&f),
            Some(NeedsHuman::OriginMismatch {
                fix: OriginFix::SetUrl,
                ..
            })
        ));
    }

    #[test]
    fn an_operation_in_progress_owns_a_detached_head() {
        let mut f = facts(
            Head::Detached {
                commit: "abc".into(),
            },
            &[b("main", O, true, Track::Even)],
        );
        for op in [InProgressOp::Rebase, InProgressOp::Bisect] {
            f.in_progress = Some(op);
            assert_eq!(
                classify(
                    &owned(Mode::Follow("main")),
                    &f,
                    &EntrySessions::idle(),
                    Refresh::Unasked
                )
                .needs_human,
                [NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op
                }]
            );
        }
        // the rest don't detach HEAD, so a detach beside one is its own reason
        for op in [
            InProgressOp::Merge,
            InProgressOp::CherryPick,
            InProgressOp::Revert,
            InProgressOp::Sequencer,
            InProgressOp::Am,
        ] {
            f.in_progress = Some(op);
            assert_eq!(
                classify(
                    &owned(Mode::Follow("main")),
                    &f,
                    &EntrySessions::idle(),
                    Refresh::Unasked
                )
                .needs_human,
                [
                    NeedsHuman::OperationInProgress {
                        checkout: "/ws/app".into(),
                        op
                    },
                    NeedsHuman::UnexpectedDetached {
                        checkout: "/ws/app".into()
                    },
                ]
            );
        }
    }

    #[test]
    fn a_worktree_that_is_a_registry_dir_is_never_removable() {
        let mut f = facts(on("main"), &[b("old", O, true, Track::Gone).unique(0)]);
        let mut wt = linked("/ws/app-old", on("old"));
        wt.submodules = Some(false);
        f.worktrees = vec![wt];
        let verdict = |f: &RepoFacts| {
            classify(
                &owned(Mode::Follow("main")),
                f,
                &EntrySessions::idle(),
                Refresh::Unasked,
            )
            .branches[0]
                .verdict
                .clone()
        };
        assert_eq!(
            verdict(&f),
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: Some("/ws/app-old".into()),
            }
        );
        f.registry_worktrees.insert("/ws/app-old".into());
        assert_eq!(
            verdict(&f),
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None,
            }
        );
    }

    #[test]
    fn each_checkout_with_an_operation_is_a_reason() {
        let mut f = facts(on("main"), &[b("main", O, true, Track::Ahead(1)).unique(1)]);
        let mut rebasing = linked(
            "/ws/app-rebasing",
            Head::Detached {
                commit: "abc".into(),
            },
        );
        rebasing.in_progress = Some(InProgressOp::Rebase);
        let mut merging = linked("/ws/app-merging", on("feat"));
        merging.in_progress = Some(InProgressOp::Merge);
        f.worktrees = vec![linked("/ws/app-quiet", on("other")), rebasing, merging];
        // gone from disk, but its git dir says a revert is mid-way
        let mut reverting = unprobed("/media/usb/app", None, UnprobedWhy::Missing);
        reverting.in_progress = Some(InProgressOp::Revert);
        f.unprobed = vec![reverting];
        f.in_progress = Some(InProgressOp::CherryPick);
        let c = classify(
            &owned(Mode::Follow("main")),
            &f,
            &EntrySessions::idle(),
            Refresh::Unasked,
        );
        assert_eq!(
            c.needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app".into(),
                    op: InProgressOp::CherryPick
                },
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app-rebasing".into(),
                    op: InProgressOp::Rebase
                },
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app-merging".into(),
                    op: InProgressOp::Merge
                },
                NeedsHuman::OperationInProgress {
                    checkout: "/media/usb/app".into(),
                    op: InProgressOp::Revert
                },
            ]
        );

        // a linked worktree's operation alone holds the entry
        f.in_progress = None;
        f.unprobed.clear();
        f.worktrees.remove(2);
        assert_eq!(
            verdicts(&owned(Mode::Follow("main")), &f),
            named(&[(
                "main",
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::Entry
                }
            )])
        );
    }

    #[test]
    fn only_the_primary_counts_for_a_detached_head() {
        let e = owned(Mode::Follow("main"));
        // a linked worktree detached is normal
        let mut f = facts(on("main"), &[b("main", O, true, Track::Even)]);
        f.worktrees = vec![linked(
            "/ws/app-detached",
            Head::Detached {
                commit: "abc".into(),
            },
        )];
        assert!(
            classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked)
                .needs_human
                .is_empty()
        );

        // a rebase in a linked worktree doesn't explain the primary's detach
        f.status.head = Head::Detached {
            commit: "abc".into(),
        };
        f.worktrees[0].in_progress = Some(InProgressOp::Rebase);
        assert_eq!(
            classify(&e, &f, &EntrySessions::idle(), Refresh::Unasked).needs_human,
            [
                NeedsHuman::OperationInProgress {
                    checkout: "/ws/app-detached".into(),
                    op: InProgressOp::Rebase
                },
                NeedsHuman::UnexpectedDetached {
                    checkout: "/ws/app".into()
                },
            ]
        );
    }

    #[test]
    fn origin_normalization() {
        let u = url("https://github.com/Me/App");
        for same in [
            "git@github.com:me/app",
            "git@github.com:me/app.git",
            "ssh://git@github.com/me/app.git",
            "https://github.com/me/app/",
            "https://token@github.com/me/app",
            "git://github.com/me/app.git",
            "git+ssh://github.com/me/app",
            "ssh+git://git@github.com/me/app",
            "git@GitHub.COM:ME/APP.git",
        ] {
            assert!(origin_matches(same, &u), "{same}");
        }
        for different in [
            "git@github.com:me/other",
            "git@gitlab.com:me/app",
            "https://github.com/them/app",
            "git://github.com/them/app",
            "git@github.com:me/app/extra",
            "git@github.com:me",
            "git@github.com:/me/app",
            " git@github.com:me/app",
            "git@github.com:me/app\n",
        ] {
            assert!(!origin_matches(different, &u), "{different}");
        }
    }

    /// URLs that name the registry's repo somewhere in their text while git
    /// connects elsewhere, or that name it in a way the registry's never
    /// does: none matches, as origin or push URL.
    const LOOKALIKE_URLS: [&str; 20] = [
        // an `@` past the authority: git connects to `evil.com`
        "ssh://evil.com/x@github.com/me/app",
        "evil.com:x@github.com/me/app",
        "ssh+git://evil.com/@github.com/me/app",
        "git@evil.com:git@github.com:me/app",
        "https://evil.com/x@github.com/me/app",
        // escapes git decodes before splitting: `evil.com` again
        "ssh://evil.com%2F@github.com/me/app",
        "ssh://git%40evil.com@github.com/me/app",
        "git@github.com:me%2Fapp",
        // an `@` left in the host
        "ssh://a@b@github.com/me/app",
        // IP literals and brackets
        "git@[::1]:me/app",
        "ssh://git@[::1]/me/app",
        "[git@github.com]:me/app",
        // a bracketed run in the user: git connects to `evil.com`
        "ssh://[evil.com]x@github.com/me/app",
        "ssh://[evil.com]x@github.com:2222/me/app",
        "[evil.com]x@github.com:me/app",
        // `@[` in the path: git's scan runs into it, connecting to `x`
        "git@github.com:me/app@[x]:y",
        // a port: the registry's URLs name none
        "ssh://git@github.com:22/me/app",
        "https://github.com:443/me/app",
        // a user that reads as an option, a scheme git spells otherwise
        "-oProxyCommand=x@github.com:me/app",
        "SSH://git@github.com/me/app",
    ];

    #[test]
    fn remote_accounts() {
        for (url, account) in [
            ("git@github.com:Me/app", Some("me")),
            ("git@github.com:me/app.git", Some("me")),
            ("ssh://git@github.com/me/app.git", Some("me")),
            ("ssh://git@host:2222/me/app", Some("me")),
            ("https://token@github.com/them/app/", Some("them")),
            ("https://gitlab.com/group/sub/app", Some("group")),
            ("gh:me/app", Some("me")),
            ("git://github.com/Me/app.git", Some("me")),
            ("/home/me/dev/app", None),
            ("../app", None),
            ("./me/app", None),
            ("file:///srv/git/app.git", None),
            ("https://github.com/me", None),
            // brackets: git connects to another host
            ("ssh://[evil.com]x@github.com:2222/me/app", None),
            ("git@github.com:me/app@[x]:y", None),
            ("", None),
        ] {
            assert_eq!(remote_account(url).as_deref(), account, "{url}");
        }
    }
}
