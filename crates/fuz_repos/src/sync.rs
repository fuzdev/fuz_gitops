//! `repos sync`: fetch, classify, and act on each branch's verdict — the
//! fast-forwards and shallow moves `status` previews.
//!
//! **The pipeline.** Sync is `status --fetch` (the same probe pool, the same
//! hardened fetch writing remote-tracking refs alone, the same visibility
//! checks) and then acts:
//!
//! 1. Probe every entry, fetching the ones `status --fetch` fetches (owned,
//!    not pinned, with an `origin` URL) first.
//! 2. Read the live sessions — after the fetches, which can take minutes,
//!    so a session started meanwhile still holds — scope them to the
//!    checkouts probed, and classify.
//! 3. Act on each branch's verdict: an `act` fast-forward or move is made;
//!    everything else is reported as it stands. Entries sharing a repo act
//!    together, one after another, each branch once; repos act in parallel.
//!
//! **Never** a push, a rebase, a merge that isn't a fast-forward, a clone,
//! a deleted branch, or a pruned worktree (the origin fetch's `--prune`
//! deletes only remote-tracking refs gone upstream); a third-party
//! reference or a pin is never touched (their verdicts never act). An entry
//! whose probe failed has no verdicts, so nothing in it acts.
//!
//! **The verdict is a plan; git is the check.** Right before each action,
//! sync re-reads the live sessions (a hold when busy detection has become
//! unavailable, or a session now works where the action would), then
//! re-checks what the action relies on, then lets git refuse whatever else
//! changed — and where git's own refusal can't cover a race, checks after
//! the fact what the action did:
//!
//! - **A fast-forward in place** — a branch no checkout has — is `git fetch
//!   . <tip>:refs/heads/<b>`, a fetch from the repo itself of the exact
//!   upstream commit: git refuses anything but a fast-forward, refuses a
//!   branch checked out (or being rebased or bisected) in any worktree, and
//!   updates the ref only if it still holds what git read. It's confined as
//!   the remote fetch is (no tags, no pruning, no `FETCH_HEAD`, no
//!   submodules, no commit-graph or bundles) and to local transport
//!   (`GIT_ALLOW_PROTOCOL=file`), so an `insteadOf` on `.` can't reach the
//!   network; the source is named by its object id, so wherever a rewrite
//!   sends it, the ref can only move to that commit. Git's fetch writes
//!   through a symbolic ref to its target, past its checked-out check, so
//!   a branch that's a symbolic ref never acts (`BranchStatus::symref`), and
//!   right before the fetch sync re-checks that it hasn't become one; one
//!   made in the instant between is the window left.
//! - **A fast-forward in a checkout** is `git merge --ff-only` of the tip,
//!   run only after the checkout's status reads it still on the branch and
//!   clean: git's merge refuses only changes it would overwrite, so a dirty
//!   tree would otherwise still move. `--no-overwrite-ignore` keeps it from
//!   replacing an ignored file (a local `.env`) with a tracked one; the
//!   branch's `mergeOptions` (`--squash`, `--autostash`, …) are overridden.
//!   The merge moves whatever branch HEAD is on when it runs, so afterwards
//!   the branch must read the tip: a HEAD switched in the instant between
//!   the status and the merge fails the action, naming what moved instead —
//!   a fast-forward only, so nothing is lost. A branch another hand moved
//!   past the tip meanwhile is held (`changed`).
//! - **A shallow move in place** is `git update-ref --no-deref <b> <tip>
//!   <old>`, after re-counting the branch's commits on no remote at `<old>`
//!   (none), finding it checked out nowhere, and finding it no symbolic
//!   ref: the update is a compare-and-swap on the commit just counted, so no
//!   commit landing meanwhile can be dropped, and it never writes through a
//!   symbolic ref (one made in the instant before is replaced by a plain
//!   ref, its target untouched). Git doesn't check a checkout for
//!   `update-ref`: a checkout switching to the branch in that instant would
//!   find its HEAD moved under its files, reading as a staged reverse diff —
//!   the files and the old commit stay, nothing is lost.
//! - **A shallow move in a checkout** is `git switch -C <b> <tip>`, after the
//!   same status check and re-count: it refuses local changes it would
//!   overwrite, an ignored file it would replace, and a branch checked out
//!   in another worktree. (`merge --ff-only` can't: the fetched tip's history
//!   stops at the shallow root. `reset --keep` would replace ignored files.)
//!   It resets the branch without a compare-and-swap, so a commit landing in
//!   the instant between the re-count and the switch would be dropped from
//!   the branch: the switch writes the branch's reflog whatever the config
//!   says, and afterwards the reflog's previous value must be the commit
//!   counted — else the action fails, naming the commit, which the reflog
//!   still holds. A branch another hand moved to the tip itself meanwhile
//!   gets no entry from the switch, and is held (`changed`): the move
//!   wasn't sync's.
//!
//! A branch deleted since classifying is held (`changed`) wherever the
//! action reads it.
//!
//! Each action moves one branch and touches at most the one checkout it's on
//! (classify holds a branch on several), re-reading what it relies on right
//! before, so actions within a repo don't depend on their order. The two
//! that rewrite a working tree run under `CHECKOUT_TIMEOUT`, not the local
//! timeout: git killed mid-checkout leaves the files half-written.
//!
//! **What runs.** The runner's hardening holds (the `git` module doc): no
//! hook, fsmonitor, or alternate-refs command runs, so nothing a fetch or
//! fast-forward brings in is executed. Programs the local config names —
//! filter drivers such as Git LFS's smudge, the gpg program
//! `merge.verifySignatures` calls — are the user's own and run as in any
//! merge or checkout they'd make.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::busy::{Detection, EntryCheckouts, Sessions, scope_sessions};
use crate::git::{CallOptions, Git, GitError};
use crate::porcelain;
use crate::probe::{
    ProbeContext, RegistryDirs, RepoFacts, RepoFetches, STATUS_ARGS, canonical, read_shallow_roots,
};
use crate::registry::Entry;
use crate::remote::RemoteFailure;
use crate::report::{BranchOutcome, BranchSync, EntryStatus, EntrySync, FetchOutcome, SyncHold};
use crate::sessions::LiveSessions;
use crate::state::{BranchStatus, Head, SyncAction, Verdict};
use crate::status::{EntryTiming, assess, probe_all, run_pool};

/// The timeout for an action that rewrites a working tree (`merge
/// --ff-only`, `switch -C`), in place of `LOCAL_TIMEOUT`.
///
/// A checkout of a large tree through the user's filter drivers (Git LFS
/// fetching content) can take minutes, and git killed mid-checkout leaves
/// the files half-written against an unmoved HEAD. Long enough for any
/// checkout that is making progress; a hung filter still ends.
pub const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How to run `sync`.
#[derive(Clone, Copy)]
pub struct SyncOptions<'a> {
    /// Git calls in flight at once — entries probed, visibility checks,
    /// repos acted on — at least one.
    pub jobs: usize,
    /// As `StatusOptions::visibility_base`: a seam for tests.
    pub visibility_base: Option<&'a str>,
    /// Reads the live sessions (`read_live_sessions`): once the fetches are
    /// done, to classify, and again right before each action. A seam for
    /// tests.
    pub read_live: &'a (dyn Fn() -> LiveSessions + Sync),
}

impl std::fmt::Debug for SyncOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncOptions")
            .field("jobs", &self.jobs)
            .field("visibility_base", &self.visibility_base)
            .finish_non_exhaustive()
    }
}

/// What sync found and did, the entries in the order given.
#[derive(Debug)]
pub struct SyncRun {
    /// What sync acted on: each entry's status after the fetch.
    pub entries: Vec<EntryStatus>,
    /// Busy detection as classified, after the fetch.
    pub sessions: Sessions,
    /// What sync did, one per entry.
    pub outcomes: Vec<EntrySync>,
    pub timings: Vec<EntryTiming>,
    /// Wall time of the probe pool (fetches included).
    pub probe_elapsed: Duration,
    /// Wall time of the acting.
    pub act_elapsed: Duration,
}

/// Fetches `entries`, classifies them, and acts on each branch's verdict.
///
/// The sessions are read after the fetch, and again before each action (the
/// module doc says how); `registry_dirs` are the whole registry's dirs,
/// whatever `entries` holds.
pub fn sync(
    entries: &[Entry],
    registry_dirs: &RegistryDirs,
    root: &Path,
    git: &Git,
    opts: SyncOptions<'_>,
) -> SyncRun {
    let start = Instant::now();
    let fetches = RepoFetches::default();
    let probes = probe_all(
        entries,
        ProbeContext {
            git,
            root,
            registry_dirs,
            fetch: true,
            fetches: &fetches,
        },
        opts.jobs,
        opts.visibility_base,
    );
    let probe_elapsed = start.elapsed();
    // after the fetches, never before: a session started while they ran holds
    let assessed = assess(entries, probes, &(opts.read_live)());

    let start = Instant::now();
    let actor = Actor {
        git,
        root,
        checkouts: &assessed.checkouts,
        read_live: opts.read_live,
    };
    let groups = repo_groups(&assessed.facts);
    let acted = run_pool(groups.len(), opts.jobs, |g| {
        actor.act_on_repo(&groups[g], &assessed.entries, &assessed.facts)
    });
    let mut branches: Vec<Vec<BranchSync>> = vec![Vec::new(); entries.len()];
    for (i, outcomes) in acted.into_iter().flatten() {
        branches[i] = outcomes;
    }
    let outcomes = assessed
        .entries
        .iter()
        .zip(&assessed.fetches)
        .zip(branches)
        .map(|((e, fetch), branches)| EntrySync {
            key: e.key.clone(),
            fetch: fetch_outcome(fetch.as_ref()),
            branches,
        })
        .collect();
    SyncRun {
        entries: assessed.entries,
        sessions: assessed.sessions,
        outcomes,
        timings: assessed.timings,
        probe_elapsed,
        act_elapsed: start.elapsed(),
    }
}

fn fetch_outcome(fetch: Option<&Result<(), RemoteFailure>>) -> FetchOutcome {
    match fetch {
        None => FetchOutcome::NotFetched,
        Some(Ok(())) => FetchOutcome::Fetched,
        Some(Err(failure)) => FetchOutcome::Failed {
            failure: failure.clone(),
        },
    }
}

/// The entries with facts, grouped by the repo they share (their common dir,
/// canonicalized when it can be), each group in entry order, the groups by
/// their first entry.
fn repo_groups(facts: &[Option<RepoFacts>]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut by_repo: HashMap<PathBuf, usize> = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        let Some(f) = f else { continue };
        let repo = canonical(&f.common_dir).unwrap_or_else(|| f.common_dir.clone());
        let g = *by_repo.entry(repo).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    groups
}

/// What acting needs: the runner, the root discovery stops at, and every
/// entry's checkouts, which the live sessions re-read before each action
/// are scoped to.
struct Actor<'a> {
    git: &'a Git,
    root: &'a Path,
    checkouts: &'a [EntryCheckouts],
    read_live: &'a (dyn Fn() -> LiveSessions + Sync),
}

impl Actor<'_> {
    /// Acts on one repo's entries, in order, returning each one's outcomes.
    ///
    /// A branch acts once for the repo. When entries sharing it disagree —
    /// one holds a branch, or leaves it to a person, that another would act
    /// on — it doesn't act, and each entry that would have reports the first
    /// one that stopped it. An outcome another entry's (`BranchSync::repeats`)
    /// names that entry.
    fn act_on_repo(
        &self,
        group: &[usize],
        statuses: &[EntryStatus],
        facts: &[Option<RepoFacts>],
    ) -> Vec<(usize, Vec<BranchSync>)> {
        // by branch: the entry whose outcome it is, and the outcome
        let mut stopped: HashMap<&str, (usize, BranchOutcome)> = HashMap::new();
        for &i in group {
            for b in &statuses[i].branches {
                if matches!(b.verdict, Verdict::Held { .. } | Verdict::NeedsHuman { .. }) {
                    stopped
                        .entry(b.name.as_str())
                        .or_insert_with(|| (i, settled(&b.verdict)));
                }
            }
        }
        let mut done: HashMap<&str, (usize, BranchOutcome)> = HashMap::new();
        let mut out = Vec::with_capacity(group.len());
        for &i in group {
            let Some(f) = &facts[i] else { continue };
            let outcomes = statuses[i]
                .branches
                .iter()
                .map(|b| {
                    let (outcome, repeats) = match &b.verdict {
                        Verdict::Act {
                            action: action @ (SyncAction::FastForward { .. } | SyncAction::Move),
                        } => match (stopped.get(b.name.as_str()), done.get(b.name.as_str())) {
                            (Some((by, o)), _) | (None, Some((by, o))) => {
                                (o.clone(), Some(statuses[*by].key.clone()))
                            }
                            (None, None) => {
                                let o = self.act(i, f, b, *action);
                                done.insert(b.name.as_str(), (i, o.clone()));
                                (o, None)
                            }
                        },
                        verdict => (settled(verdict), None),
                    };
                    BranchSync {
                        name: b.name.clone(),
                        outcome,
                        repeats,
                    }
                })
                .collect();
            out.push((i, outcomes));
        }
        out
    }

    /// Takes `action` on branch `b` of entry `i`, re-checking first.
    fn act(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        action: SyncAction,
    ) -> BranchOutcome {
        let held = |by| BranchOutcome::Held { action, by };
        let failed = |message: String| BranchOutcome::Failed { action, message };
        let Some(upstream) = facts
            .branches
            .iter()
            .find(|f| f.branch.name == b.name)
            .and_then(|f| f.branch.upstream_ref.as_deref())
        else {
            return failed(format!("{} has no upstream to move to", b.name));
        };
        // the probed checkouts on it, from the facts classify read: it held
        // a branch on several, or on an unprobed one
        let on_branch = |head: &Head| matches!(head, Head::Branch { name } if *name == b.name);
        let mut on: Vec<&str> = Vec::new();
        if on_branch(&facts.status.head) {
            on.push(&facts.path);
        }
        on.extend(
            facts
                .worktrees
                .iter()
                .filter(|c| on_branch(&c.head))
                .map(|c| c.path.as_str()),
        );
        debug_assert!(
            on.len() <= 1,
            "{} acts on several checkouts: {on:?}",
            b.name
        );
        let checkout = match on[..] {
            [] => None,
            [c] => Some(c),
            // unreachable, but never a guess at which checkout
            _ => return failed(format!("{} is checked out in several checkouts", b.name)),
        };
        if let Some(by) = self.busy_now(i, &b.name, checkout) {
            return held(by);
        }
        let step = Step {
            git: self.git,
            opts: CallOptions {
                ceiling: Some(self.root),
                ..CallOptions::default()
            },
            branch: &b.name,
            upstream,
            common_dir: &facts.common_dir,
        };
        let result = match (action, checkout) {
            (SyncAction::FastForward { .. }, None) => step.ff_in_place(Path::new(&facts.path)),
            (SyncAction::FastForward { .. }, Some(c)) => step.ff_in_checkout(Path::new(c)),
            (SyncAction::Move, None) => step.move_in_place(Path::new(&facts.path)),
            (SyncAction::Move, Some(c)) => step.move_in_checkout(Path::new(c)),
            (SyncAction::Push { .. }, _) => return held(SyncHold::NotPushed),
        };
        match result {
            Ok(Done::Updated { from, to }) if matches!(action, SyncAction::Move) => {
                BranchOutcome::Moved { from, to }
            }
            Ok(Done::Updated { from, to }) => BranchOutcome::FastForwarded { from, to },
            Ok(Done::AlreadyThere) => BranchOutcome::Untouched,
            Ok(Done::Held(by)) => held(by),
            Err(message) => failed(message),
        }
    }

    /// What holds an action on `branch` now, from the live sessions re-read:
    /// detection unavailable, or a session that may be on the branch
    /// through a git dir no worktree list names, holds any action; one in
    /// the checkout it's on, or a checkout whose path can't be resolved,
    /// holds that checkout's.
    fn busy_now(&self, i: usize, branch: &str, checkout: Option<&str>) -> Option<SyncHold> {
        let live = (self.read_live)();
        let (_, per_entry) = scope_sessions(&live, self.checkouts);
        let sessions = &per_entry[i];
        if sessions.detection == Detection::Unavailable || sessions.unlisted_on(branch) > 0 {
            return Some(SyncHold::BusyUnknown);
        }
        let checkout = checkout?;
        if !sessions.at(checkout).is_empty() {
            Some(SyncHold::Busy)
        } else if sessions.unresolved_at(checkout) {
            Some(SyncHold::BusyUnknown)
        } else {
            None
        }
    }
}

/// A verdict that doesn't act, as an outcome; an `act` on a push is held,
/// since sync never pushes.
fn settled(verdict: &Verdict) -> BranchOutcome {
    match verdict {
        Verdict::Quiet | Verdict::LocalOnly | Verdict::Cleanup { .. } => BranchOutcome::Untouched,
        Verdict::NeedsHuman { reason } => BranchOutcome::NeedsHuman { reason: *reason },
        Verdict::Held { action, by } => BranchOutcome::Held {
            action: *action,
            by: (*by).into(),
        },
        Verdict::Act { action } => BranchOutcome::Held {
            action: *action,
            by: SyncHold::NotPushed,
        },
    }
}

/// How an action went, short of failing.
#[derive(Debug)]
enum Done {
    /// The branch moved from `from` to `to`.
    Updated { from: String, to: String },
    /// The branch already held the tip.
    AlreadyThere,
    /// A re-check held it.
    Held(SyncHold),
}

/// One action's git calls: on `branch`, toward `upstream`'s tip.
struct Step<'a> {
    git: &'a Git,
    opts: CallOptions<'a>,
    branch: &'a str,
    /// The resolved upstream ref, `refs/remotes/origin/<b>`.
    upstream: &'a str,
    common_dir: &'a Path,
}

/// The confined local fetch's flags before `.` and its refspec: what the
/// remote fetch (`probe`'s `FETCH_ARGS`) switches off, plus `FETCH_HEAD`.
/// `--porcelain` prints what moved, old and new ids exact.
const LOCAL_FETCH_ARGS: [&str; 10] = [
    "-c",
    "fetch.bundleURI=",
    "fetch",
    "--porcelain",
    "--no-write-fetch-head",
    "--no-tags",
    "--no-prune",
    "--no-prune-tags",
    "--recurse-submodules=no",
    // no `--update-head-ok`: git refuses a branch checked out anywhere
    "--no-write-commit-graph",
];

impl Step<'_> {
    /// The commit `rev` names in `dir`.
    fn resolve(&self, dir: &Path, rev: &str) -> Result<String, String> {
        let rev = format!("{rev}^{{commit}}");
        self.git
            .output_string(
                dir,
                &["rev-parse", "--verify", "--end-of-options", &rev],
                self.opts,
            )
            .map(|s| s.trim().to_owned())
            .map_err(|e| git_message(&e))
    }

    fn local(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }

    /// The commit the branch holds in `dir`, or `None` when the branch no
    /// longer exists (deleted since classifying); any other failure to
    /// resolve it stays an error.
    fn resolve_local(&self, dir: &Path) -> Result<Option<String>, String> {
        let local = self.local();
        let err = match self.resolve(dir, &local) {
            Ok(commit) => return Ok(Some(commit)),
            Err(err) => err,
        };
        let out = self
            .git
            .run(
                dir,
                &["show-ref", "--exists", "--end-of-options", &local],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        // `--exists`: 2 for a ref that doesn't exist, and only for that
        if out.status.code() == Some(2) {
            Ok(None)
        } else {
            Err(err)
        }
    }

    /// Whether `ancestor` is `commit` or one of its ancestors.
    fn is_ancestor(&self, dir: &Path, ancestor: &str, commit: &str) -> Result<bool, String> {
        let out = self
            .git
            .run(
                dir,
                &["merge-base", "--is-ancestor", ancestor, commit],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(first_message(&out.stderr)),
        }
    }

    /// Whether the branch is a symbolic ref now, which a write to it would
    /// go through to its target.
    fn is_symref(&self, dir: &Path) -> Result<bool, String> {
        let out = self
            .git
            .run(dir, &["symbolic-ref", "-q", &self.local()], self.opts)
            .map_err(|e| git_message(&e))?;
        // `-q`: 1 for a plain ref (or none), silently
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(first_message(&out.stderr)),
        }
    }

    /// Fast-forwards a branch no checkout has to the upstream's tip, with a
    /// fetch from the repo itself (the module doc says why).
    fn ff_in_place(&self, dir: &Path) -> Result<Done, String> {
        let to = self.resolve(dir, self.upstream)?;
        let local = self.local();
        let Some(from) = self.resolve_local(dir)? else {
            // deleted since classifying
            return Ok(Done::Held(SyncHold::Changed));
        };
        if from == to {
            return Ok(Done::AlreadyThere);
        }
        // the fetch would write through it, unchecked
        if self.is_symref(dir)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        let refspec = format!("{to}:{local}");
        let mut args = LOCAL_FETCH_ARGS.to_vec();
        args.extend([".", refspec.as_str()]);
        let opts = CallOptions {
            allow_protocol: Some("file"),
            ..self.opts
        };
        let out = self
            .git
            .run(dir, &args, opts)
            .map_err(|e| git_message(&e))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // `<flag> <old> <new> <ref>`, one per ref it considered, the flag a
        // single char (a space for a fast-forward)
        let line = stdout.lines().find_map(|l| {
            let (flag, rest) = (l.get(..1)?, l.get(1..)?.strip_prefix(' ')?);
            let mut f = rest.splitn(3, ' ');
            let (old, new, r) = (f.next()?, f.next()?, f.next()?);
            (r == local).then_some((flag, old, new))
        });
        if !out.status.success() {
            return Err(match line {
                Some(("!", ..)) => format!("git rejected moving {local}: not a fast-forward"),
                _ => first_message(&out.stderr),
            });
        }
        // what git says it did, checked against the ref itself
        let Some(now) = self.resolve_local(dir)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        match line {
            Some((" ", old, new)) if new == to && now == to => Ok(Done::Updated {
                from: old.to_owned(),
                to,
            }),
            // up to date: moved there meanwhile
            None if now == to => Ok(Done::AlreadyThere),
            _ => Err(format!(
                "the fetch left {local} at {now}, not {to}: {}",
                stdout.trim()
            )),
        }
    }

    /// Fast-forwards the branch in `checkout`, the one it's on, when it's
    /// still there and clean.
    fn ff_in_checkout(&self, checkout: &Path) -> Result<Done, String> {
        if let Some(by) = self.checkout_changed(checkout)? {
            return Ok(Done::Held(by));
        }
        let Some(from) = self.resolve_local(checkout)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        let to = self.resolve(checkout, self.upstream)?;
        if from == to {
            return Ok(Done::AlreadyThere);
        }
        self.merge_ff(checkout, from, to)
    }

    /// `merge --ff-only` of `to` in `checkout`, then the check that it moved
    /// the branch from `from`: the merge moves whatever HEAD is on when it
    /// runs, and a HEAD switched since the status read it fails the action.
    /// A branch another hand moved past `to` meanwhile (git's "Already up
    /// to date") is held, not failed: nothing of it is lost.
    fn merge_ff(&self, checkout: &Path, from: String, to: String) -> Result<Done, String> {
        self.run_checkout(
            checkout,
            &[
                "-c",
                "submodule.recurse=false",
                "merge",
                "--ff-only",
                "--no-overwrite-ignore",
                "--no-autostash",
                "--no-squash",
                "--no-stat",
                "--quiet",
                &to,
            ],
        )?;
        let local = self.local();
        let Some(now) = self.resolve_local(checkout)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        if now == to {
            return Ok(Done::Updated { from, to });
        }
        let head = self
            .git
            .output_string(checkout, &["symbolic-ref", "-q", "HEAD"], self.opts)
            .map_or_else(|_| "a detached HEAD".to_owned(), |s| s.trim().to_owned());
        if head == local && self.is_ancestor(checkout, &to, &now)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        Err(format!(
            "HEAD was on {head} when the merge ran; {local} is at {now}, not {to}"
        ))
    }

    /// Moves a shallow branch no checkout has to the upstream's tip, when it
    /// still has nothing on no remote: a compare-and-swap on the commit
    /// counted. A checkout switching to the branch in the instant between
    /// the re-checks and the update finds its HEAD moved under its files: a
    /// staged reverse diff, nothing lost.
    fn move_in_place(&self, dir: &Path) -> Result<Done, String> {
        let local = self.local();
        let Some(from) = self.resolve_local(dir)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        let to = self.resolve(dir, self.upstream)?;
        if from == to {
            return Ok(Done::AlreadyThere);
        }
        if self.has_local_work(dir, &from)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        // checked out nowhere, as git records it now
        let at = self
            .git
            .output_string(
                dir,
                &["for-each-ref", "--format=%(worktreepath)", &local],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        if !at.trim().is_empty() || self.is_symref(dir)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        self.run_in(
            dir,
            &[
                "update-ref",
                "--no-deref",
                "-m",
                "repos sync: move to the fetched tip",
                &local,
                &to,
                &from,
            ],
            self.opts,
        )?;
        Ok(Done::Updated { from, to })
    }

    /// Moves a shallow branch in `checkout`, the one it's on, when it's
    /// still there and clean and has nothing on no remote.
    fn move_in_checkout(&self, checkout: &Path) -> Result<Done, String> {
        if let Some(by) = self.checkout_changed(checkout)? {
            return Ok(Done::Held(by));
        }
        let Some(from) = self.resolve_local(checkout)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        let to = self.resolve(checkout, self.upstream)?;
        if from == to {
            return Ok(Done::AlreadyThere);
        }
        if self.has_local_work(checkout, &from)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        self.switch_reset(checkout, from, to)
    }

    /// `switch -C` to `to` in `checkout`, then the check that the branch it
    /// reset was at `from`, the commit counted: the switch is no
    /// compare-and-swap, so a commit made in between is dropped from the
    /// branch, and the reflog — written whatever the config says — is where
    /// it's found. A branch another hand moved to exactly `to` meanwhile
    /// gets no reflog entry from the switch (it moved nothing), so it's held:
    /// the move wasn't sync's.
    fn switch_reset(&self, checkout: &Path, from: String, to: String) -> Result<Done, String> {
        self.run_checkout(
            checkout,
            &[
                "-c",
                "submodule.recurse=false",
                "-c",
                "core.logAllRefUpdates=true",
                "switch",
                "--no-overwrite-ignore",
                "--no-guess",
                "--quiet",
                "-C",
                self.branch,
                &to,
            ],
        )?;
        let local = self.local();
        let Some(now) = self.resolve_local(checkout)? else {
            return Ok(Done::Held(SyncHold::Changed));
        };
        if now != to {
            return Err(format!(
                "{local} moved again after the switch to {to}: it's at {now}"
            ));
        }
        // the switch's own entry, `branch: Reset to <to>`, or none: it was
        // already there
        let newest = self
            .git
            .output_string(
                checkout,
                &[
                    "log",
                    "-g",
                    "-1",
                    "--no-show-signature",
                    "--format=%gs",
                    "--end-of-options",
                    &local,
                    "--",
                ],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        if newest.trim() != format!("branch: Reset to {to}") {
            return Ok(Done::Held(SyncHold::Changed));
        }
        let before = self.resolve(checkout, &format!("{local}@{{1}}"))?;
        if before == from {
            Ok(Done::Updated { from, to })
        } else {
            Err(format!(
                "{local} was at {before}, not {from}, when the switch moved it to {to}: \
                 {before} is in its reflog (git reflog {local})"
            ))
        }
    }

    /// Why the checkout can't take the action now: its HEAD left the branch
    /// (`Changed`), or it has uncommitted changes (`DirtyCheckout`) — read
    /// fresh, as `status` reads a checkout. A branch made a symbolic ref
    /// reads as its target, so as `Changed`.
    fn checkout_changed(&self, checkout: &Path) -> Result<Option<SyncHold>, String> {
        let out = self
            .git
            .output(checkout, &STATUS_ARGS, self.opts)
            .map_err(|e| git_message(&e))?;
        let status = porcelain::parse_status(&out)?;
        Ok(
            if !matches!(&status.head, Head::Branch { name } if name == self.branch) {
                Some(SyncHold::Changed)
            } else if !status.uncommitted.is_clean() {
                Some(SyncHold::DirtyCheckout)
            } else {
                None
            },
        )
    }

    /// Whether `commit` has commits on no remote-tracking ref, a shallow
    /// root aside — fetched, not made here — as the probe counts them.
    fn has_local_work(&self, dir: &Path, commit: &str) -> Result<bool, String> {
        let out = self
            .git
            .output_string(dir, &["rev-list", commit, "--not", "--remotes"], self.opts)
            .map_err(|e| git_message(&e))?;
        let roots = read_shallow_roots(self.common_dir);
        Ok(out.lines().any(|c| !roots.contains(c)))
    }

    /// Runs git in `checkout` to rewrite its working tree, under
    /// `CHECKOUT_TIMEOUT`.
    fn run_checkout(&self, checkout: &Path, args: &[&str]) -> Result<(), String> {
        let opts = CallOptions {
            timeout: Some(CHECKOUT_TIMEOUT),
            ..self.opts
        };
        self.run_in(checkout, args, opts)
    }

    /// Runs git in `dir`, failing with git's message.
    fn run_in(&self, dir: &Path, args: &[&str], opts: CallOptions<'_>) -> Result<(), String> {
        let out = self.git.run(dir, args, opts).map_err(|e| git_message(&e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(first_message(&out.stderr))
        }
    }
}

/// A git call's failure as an outcome's message: git's own words when it
/// ran and refused.
fn git_message(e: &GitError) -> String {
    match e {
        GitError::Failed { stderr, .. } => first_message(stderr),
        e => e.to_string(),
    }
}

/// Git's first `fatal:` or `error:` line, else its first line, else a
/// placeholder for a git that said nothing.
fn first_message(stderr: &str) -> String {
    let lines = || stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .or_else(|| lines().next())
        .map_or_else(|| "git failed without a message".to_owned(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_s_first_error_line_is_the_message() {
        assert_eq!(
            first_message(
                "Updating a..b\nerror: Your local changes to the following files would be \
                 overwritten by merge:\n\tf\nAborting\n"
            ),
            "error: Your local changes to the following files would be overwritten by merge:"
        );
        assert_eq!(first_message("\n  hint: x\n"), "hint: x");
        assert_eq!(first_message(""), "git failed without a message");
    }

    // the checks after the fact, driven from just past the re-checks: the
    // races they catch fall between a re-check and git's write, which no
    // seam in `sync` reaches

    /// A repo in a tempdir, no global or system config, reflogs off — so
    /// what a branch's reflog holds, sync wrote.
    struct Repo {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    }

    impl Repo {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("app");
            let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_AUTHOR_NAME".into(), "a".into()),
                ("GIT_AUTHOR_EMAIL".into(), "a@example.com".into()),
                ("GIT_COMMITTER_NAME".into(), "a".into()),
                ("GIT_COMMITTER_EMAIL".into(), "a@example.com".into()),
            ];
            env.extend(std::env::var_os("PATH").map(|p| ("PATH".into(), p)));
            let repo = Self {
                _tmp: tmp,
                dir,
                env,
            };
            std::fs::create_dir(&repo.dir).unwrap();
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "core.logAllRefUpdates", "false"]);
            repo.commit("root");
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .env_clear()
                .envs(self.env.iter().map(|(k, v)| (k, v)))
                .current_dir(&self.dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        }

        /// Commits a new file `name` on HEAD.
        fn commit(&self, name: &str) -> String {
            std::fs::write(self.dir.join(name), name).unwrap();
            self.git(&["add", name]);
            self.git(&["commit", "-q", "-m", name]);
            self.git(&["rev-parse", "HEAD"])
        }

        /// A commit on `parent`'s tree with `parent` as its parent, made
        /// without touching HEAD or the files.
        fn child_of(&self, parent: &str) -> String {
            let tree = format!("{parent}^{{tree}}");
            self.git(&["commit-tree", &tree, "-p", parent, "-m", "upstream"])
        }

        fn runner(&self) -> Git {
            Git::with_clean_env(self.env.clone())
        }
    }

    fn step<'a>(git: &'a Git, branch: &'a str, common_dir: &'a Path) -> Step<'a> {
        Step {
            git,
            opts: CallOptions::default(),
            branch,
            upstream: "refs/remotes/origin/unused",
            common_dir,
        }
    }

    #[test]
    fn a_commit_the_switch_dropped_fails_the_move() {
        let repo = Repo::new();
        let counted = repo.git(&["rev-parse", "main"]);
        // landed after the re-count
        let landed = repo.commit("landed");
        let tip = repo.child_of(&counted);
        let git = repo.runner();
        let common_dir = repo.dir.join(".git");
        let step = step(&git, "main", &common_dir);

        let Err(message) = step.switch_reset(&repo.dir, counted, tip.clone()) else {
            panic!("the dropped commit went unreported");
        };
        assert!(message.contains(&landed), "{message}");
        assert!(message.contains("git reflog refs/heads/main"), "{message}");
        // moved all the same, and the reflog holds what it dropped
        assert_eq!(repo.git(&["rev-parse", "main"]), tip);
        assert_eq!(repo.git(&["rev-parse", "main@{1}"]), landed);

        // the control: at the commit counted, the move stands
        let next = repo.child_of(&tip);
        assert!(matches!(
            step.switch_reset(&repo.dir, tip.clone(), next.clone()),
            Ok(Done::Updated { from, to }) if from == tip && to == next
        ));
    }

    #[test]
    fn a_head_switched_before_the_merge_fails_the_fast_forward() {
        let repo = Repo::new();
        let from = repo.git(&["rev-parse", "main"]);
        let tip = repo.child_of(&from);
        repo.git(&["branch", "feat"]);
        // switched away after the status read it on `feat`
        repo.git(&["switch", "-q", "-c", "other"]);
        let git = repo.runner();
        let common_dir = repo.dir.join(".git");
        let step = step(&git, "feat", &common_dir);

        let Err(message) = step.merge_ff(&repo.dir, from.clone(), tip.clone()) else {
            panic!("the merge's move of another branch went unreported");
        };
        assert!(
            message.contains(&format!(
                "HEAD was on refs/heads/other when the merge ran; refs/heads/feat is at \
                 {from}, not {tip}"
            )),
            "{message}"
        );
        // the merge moved the branch HEAD was on, by a fast-forward
        assert_eq!(repo.git(&["rev-parse", "other"]), tip);
        assert_eq!(repo.git(&["rev-parse", "feat"]), from);

        // the control: on the branch, the fast-forward stands
        repo.git(&["switch", "-q", "feat"]);
        assert!(matches!(
            step.merge_ff(&repo.dir, from.clone(), tip.clone()),
            Ok(Done::Updated { from: f, to }) if f == from && to == tip
        ));
    }

    #[test]
    fn a_branch_moved_past_the_tip_before_the_merge_is_held() {
        let repo = Repo::new();
        let from = repo.git(&["rev-parse", "main"]);
        let tip = repo.child_of(&from);
        // another hand fast-forwards past the tip after the status read it
        let past = repo.child_of(&tip);
        repo.git(&["merge", "-q", "--ff-only", &past]);
        let git = repo.runner();
        let common_dir = repo.dir.join(".git");
        let step = step(&git, "main", &common_dir);

        // git's "Already up to date", and nothing lost
        assert!(matches!(
            step.merge_ff(&repo.dir, from, tip),
            Ok(Done::Held(SyncHold::Changed))
        ));
        assert_eq!(repo.git(&["rev-parse", "main"]), past);
    }

    #[test]
    fn a_branch_moved_to_the_tip_by_another_hand_is_held_not_moved() {
        // with no reflog: the switch finds the branch there and writes none
        let repo = Repo::new();
        let counted = repo.git(&["rev-parse", "main"]);
        let tip = repo.child_of(&counted);
        repo.git(&["update-ref", "refs/heads/main", &tip]);
        let git = repo.runner();
        let common_dir = repo.dir.join(".git");
        let step = step(&git, "main", &common_dir);

        assert!(matches!(
            step.switch_reset(&repo.dir, counted, tip.clone()),
            Ok(Done::Held(SyncHold::Changed))
        ));
        assert_eq!(repo.git(&["rev-parse", "main"]), tip);

        // with one: its newest entry is the other hand's, whose previous
        // value is the commit counted, so it would pass for the switch's
        let next = repo.child_of(&tip);
        repo.git(&[
            "update-ref",
            "--create-reflog",
            "-m",
            "another hand",
            "refs/heads/main",
            &next,
        ]);
        assert_eq!(repo.git(&["rev-parse", "main@{1}"]), tip);
        assert!(matches!(
            step.switch_reset(&repo.dir, tip, next.clone()),
            Ok(Done::Held(SyncHold::Changed))
        ));
        assert_eq!(repo.git(&["rev-parse", "main"]), next);
    }
}
