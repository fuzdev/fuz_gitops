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
    AtRest, BranchHold, BranchNeedsHuman, BranchStatus, CleanupReason, CloneHold, CloneRecipe,
    CloneVerdict, Head, InProgressOp, Prune, PruneLoss, RefreshHold, RefreshVerdict, Relation,
    SyncAction, Uncommitted, UnprobedWhy, UnprobedWorktree, UnprobedWorktreeStatus, Verdict,
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
    /// registry's HTTPS URL. The refresh is held
    /// (`RefreshHold::OriginNotHttps`), never fetched; the rest of the entry
    /// goes on. `fix` is how to point `origin` at `expected`, as
    /// `origin_mismatch`'s says — `None` when a `url.<base>.insteadOf`
    /// rewrite changes origin's URL, which setting the URL may not undo: the
    /// rewrite is what to change.
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
        /// `None` when it can't be read.
        head: Option<Head>,
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
    /// meant. Its clone is held (`CloneHold::Entry`); a person clones it, or
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
    /// clone is held (`CloneHold::Entry`); a person renames the dir to the
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
    pub(crate) const fn holds_entry(&self) -> bool {
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
    fn decide(config: &ConfigFacts) -> Self {
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
pub(crate) struct Classified {
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
        Some(Head::Branch { name }) => {
            if !facts.branches.iter().any(|b| b.branch.name == *name) {
                losses.push(PruneLoss::MissingBranch { name: name.clone() });
            }
        }
        Some(Head::Detached { .. }) => losses.push(PruneLoss::DetachedHead),
        None => losses.push(PruneLoss::UnknownHead),
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
pub(crate) const fn refresh_intent(entry: &Entry, refresh: Refresh) -> Option<RefreshVerdict> {
    match refresh {
        Refresh::Named if entry.pinned => Some(RefreshVerdict::Held {
            by: RefreshHold::Pinned,
        }),
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
/// (`RefreshHold::Entry`, the entry's `origin_mismatch` reason) — never
/// fetched, since the fetch would bring in another repo's history, over
/// whatever transport that URL names. So is one whose fetch wouldn't reach
/// the registry's repo over HTTPS (`fetches_over_https`: an SSH-form
/// origin, or an `insteadOf` rewrite), held by
/// `RefreshHold::OriginNotHttps` (the entry's `origin_not_https` reason): a
/// reference is fetched over HTTPS alone, and that fetch would fail.
/// The probe decides its fetch by this verdict.
pub(crate) fn refresh_verdict(
    entry: &Entry,
    refresh: Refresh,
    config: &ConfigFacts,
) -> Option<RefreshVerdict> {
    match refresh_intent(entry, refresh) {
        Some(RefreshVerdict::Act) if origin_drift(entry, config).is_some() => {
            Some(RefreshVerdict::Held {
                by: RefreshHold::Entry,
            })
        }
        Some(RefreshVerdict::Act) if !fetches_over_https(entry, config) => {
            Some(RefreshVerdict::Held {
                by: RefreshHold::OriginNotHttps,
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
pub(crate) fn fetch_url_mismatch<'a>(entry: &Entry, config: &'a ConfigFacts) -> Option<&'a str> {
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
pub(crate) fn lazy_transport(origin: &str) -> Option<&'static str> {
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
pub(crate) fn classify(
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
pub(crate) struct ClassifiedMissing {
    pub clone: CloneVerdict,
    pub needs_human: Vec<NeedsHuman>,
}

/// Classifies an entry whose dir is missing: sync clones it
/// (`clone_recipe`), unless something at that path, or the registry, holds
/// it.
///
/// Another entry naming the same repo holds it for a person
/// (`clone_shares_repo`, `CloneHold::Entry`): the missing dir may have been a
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
pub(crate) fn classify_missing(
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
        Some(CloneHold::Entry)
    } else if busy {
        Some(CloneHold::Busy)
    } else if recorded_worktree {
        Some(CloneHold::UnprobedWorktree)
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
fn clone_recipe(entry: &Entry) -> CloneRecipe {
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
    fn of(&self, action: SyncAction) -> Option<BranchHold> {
        let push = matches!(action, SyncAction::Push { .. });
        if self.pinned {
            Some(BranchHold::Pinned)
        } else if self.entry {
            Some(BranchHold::Entry)
        } else if push && self.push_url {
            Some(BranchHold::PushUrl)
        } else if self.fetch_failed {
            Some(BranchHold::FetchFailed)
        } else if self.on.busy {
            Some(BranchHold::Busy)
        } else if !push && self.on.dirty {
            Some(BranchHold::DirtyCheckout)
        } else if !push && self.on.unprobed {
            Some(BranchHold::UnprobedWorktree)
        } else if !push && self.on.several {
            Some(BranchHold::SeveralCheckouts)
        } else if self.on.maybe_busy || self.detection == Detection::Unavailable {
            Some(BranchHold::BusyUnknown)
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
            Some(Head::Branch { name: n }) => n == name,
            Some(Head::Detached { .. }) => false,
            None => true,
        })
        .map(|u| u.path.as_str())
        .collect();
    folded.head_unknown = facts.unprobed.iter().any(|u| u.head.is_none());
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
            by: RefreshHold::OriginNotHttps,
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
    // that can't be read (an unlisted worktree whose worktree git dir can't be
    // looked up is that git dir) is that reason's to name, and it holds
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
fn origin_drift(entry: &Entry, config: &ConfigFacts) -> Option<OriginRemote> {
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
pub(crate) fn push_target(b: &RefFacts) -> Option<&str> {
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
pub(crate) fn push_urls_match(urls: &[String], url: &RepoUrl) -> bool {
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
pub(crate) fn origin_matches(origin: &str, url: &RepoUrl) -> bool {
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
pub(crate) fn remote_account(url: &str) -> Option<String> {
    let p = remote_parts(url)?;
    let mut parts = p.path.split('/');
    let (Some(account), Some(name)) = (parts.next(), parts.next()) else {
        return None;
    };
    let named = |s: &str| !s.is_empty() && s != "." && s != "..";
    (named(account) && named(name)).then(|| account.to_ascii_lowercase())
}

#[cfg(test)]
mod tests;
