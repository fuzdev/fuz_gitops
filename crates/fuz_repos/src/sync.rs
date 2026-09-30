//! `repos sync`: fetch, classify, and act on each branch's verdict — the
//! fast-forwards, shallow moves, and pushes `status` previews — and on each
//! missing entry's, cloning it.
//!
//! **The pipeline.** Sync is `status --fetch` (the same probe pool, the same
//! hardened fetch writing remote-tracking refs alone, the same visibility
//! checks) and then acts:
//!
//! 1. Probe every entry, fetching the ones `status --fetch` fetches (owned,
//!    not pinned, or a third-party reference the run refreshes whose origin
//!    is the registry's repo — over HTTPS alone — with an `origin` URL)
//!    first.
//! 2. Read the live sessions — after the fetches, which can take minutes,
//!    so a session started meanwhile still holds — scope them to the
//!    checkouts probed, and classify.
//! 3. Act on each branch's verdict: an `act` fast-forward, move, or push is
//!    made; everything else is reported as it stands. Entries sharing a repo
//!    act together, one after another, each branch once; repos act in
//!    parallel, and so do clones (`clone`), each its own entry's — entry
//!    dirs are plain names under the root, no two alike, so no clone lands
//!    in another's.
//!
//! **Never** a force-push, a tag pushed, a remote branch created on
//! purpose, a rebase, a merge that isn't a fast-forward, a clone over
//! anything at an entry's path, a deleted branch, or a pruned worktree (the
//! origin fetch's `--prune` deletes only remote-tracking refs gone
//! upstream); a pin, once there, is never touched (its verdicts never
//! act), and a third-party reference only when the run refreshes it —
//! named as a target, or under `--references` — and then never pushed. An
//! entry whose probe failed has no verdicts, so nothing in it acts. An
//! agent's pushes are held (`Caller::Agent`, `HeldBy::Gateway`) until the
//! gateway lands: a person runs sync to push.
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
//! - **A push** is `git push origin <oid>:<ref>`: the commit the branch held
//!   when probed, to its upstream's ref on origin (`push_target`: a branch,
//!   never `refs/heads/HEAD`), so a commit landing after classifying is never
//!   pushed unseen. Right before, sync re-reads the branch — the same commit,
//!   upstream, and ref on origin, no symbolic ref, else `changed` — re-reads
//!   where a push through origin goes (`git remote get-url --push --all`,
//!   `pushurl` and `pushInsteadOf` applied: exactly one URL, the registry's
//!   repo over SSH as `push_urls_match` reads it — the host git connects to
//!   and the path there, never the URL's text — else held `push_url`), and
//!   re-counts the commits ahead of the remote-tracking ref (the same count,
//!   the ref an ancestor, else `changed`). The push is a plain one — no force,
//!   no lease: a lease is git's force with a check, which would replace git's
//!   own fast-forward refusal with the tool's ancestor check, and git's
//!   refusal stays the last check — with no tags, no submodules, no push
//!   options, no push certificate, and git's own remote command
//!   (`PUSH_ARGS`), over SSH only
//!   (`GIT_ALLOW_PROTOCOL=ssh`), batch-mode as the fetch. Git refuses anything
//!   but a fast-forward of what the remote holds, so a remote moved since the
//!   fetch is held (`changed`) for a rerun; the remote's own refusal (a
//!   ruleset, a hook) or a host unreachable fails (`push_failed`, classified).
//!   Git records the push in origin's remote-tracking ref for the pushed
//!   branch, the one the fetch writes; a failed or refused fetch holds every
//!   push, so that write goes through refspecs the fetch confined. A remote
//!   branch deleted in the instant between the fetch and the push is made anew
//!   (`pushed` with no `from`), and one deleted and recreated at an ancestor
//!   of the commit is fast-forwarded: the one race a lease would refuse and a
//!   plain push can't.
//!
//! - **A clone** of a missing entry (the `clone` module doc has the recipe)
//!   is made in a temp dir beside the entry's and moved into place only
//!   when whole, never over anything there. Right before, sync re-reads the
//!   live sessions — one now working at or under the path holds it (`busy`)
//!   — and re-checks that nothing is at the path (`changed`). Busy
//!   detection that's unavailable holds no clone, and neither does an agent
//!   running the tool: a clone writes a new dir, and no remote.
//!
//! A branch deleted since classifying is held (`changed`) wherever the
//! action reads it.
//!
//! **A partial clone** (a sparse reference, cloned `--filter=blob:none`)
//! lacks the blobs a new tip's checkout needs: the two actions that
//! rewrite a working tree fetch them on demand (`LazyFetch`), from origin
//! alone over the transport its URL names (`lazy_transport`), writing
//! objects and no ref. Every other call keeps lazy fetching off.
//!
//! Each action moves one branch and touches at most the one checkout it's on
//! (classify holds a fast-forward or move on several; a push touches none),
//! re-reading what it relies on right before, so actions within a repo
//! don't depend on their order. The two
//! that rewrite a working tree run under `CHECKOUT_TIMEOUT`, not the local
//! timeout: git killed mid-checkout leaves the files half-written.
//!
//! **What runs.** The runner's hardening holds (the `git` module doc): no
//! hook, fsmonitor, or alternate-refs command runs, so nothing a fetch or
//! fast-forward brings in is executed — a push's `pre-push` and
//! `reference-transaction` hooks included. Programs the local config names —
//! filter drivers such as Git LFS's smudge, the gpg program
//! `merge.verifySignatures` calls — are the user's own and run as in any
//! merge or checkout they'd make.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::busy::{Detection, EntryCheckouts, Sessions, scope_sessions, sessions_under};
use crate::classify::{Refresh, push_target, push_urls_match};
use crate::clone::Cloner;
use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::porcelain::{self, ConfigFacts};
use crate::probe::{
    ProbeContext, RegistryDirs, RepoFacts, RepoFetches, STATUS_ARGS, canonical, read_push_urls,
    read_shallow_roots,
};
use crate::registry::{Entry, RepoUrl};
use crate::remote::{RefspecContext, RemoteFailure};
use crate::report::{
    BranchOutcome, BranchSync, CloneOutcome, EntryStatus, EntrySync, FetchOutcome, SyncHold,
    UnregisteredClone,
};
use crate::sessions::{Caller, LiveSessions};
use crate::state::{BranchStatus, CloneRecipe, CloneVerdict, Head, SyncAction, Verdict};
use crate::status::{Assess, EntryTiming, assess, probe_all, run_pool};
use crate::url::remote_parts;

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
    /// Who runs the tool (`Caller::from_env`): an agent's pushes are held
    /// for the gateway.
    pub caller: Caller,
    /// A clone's timeout: `CLONE_TIMEOUT`, but in tests.
    pub clone_timeout: Duration,
    /// Which references the run refreshes (`Refresh`): a third-party one
    /// fetched over HTTPS and acted on, a pin named refused, and one whose
    /// origin isn't the registry's repo, or isn't reached over HTTPS, held
    /// (`refresh_verdict`).
    pub refresh: Refresh,
    /// The unregistered scan's dirs, when it ran (without targets, or with
    /// a missing entry among them): a missing entry whose repo one of them
    /// clones is held, never cloned.
    pub unregistered: Option<&'a [UnregisteredClone]>,
}

impl std::fmt::Debug for SyncOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncOptions")
            .field("jobs", &self.jobs)
            .field("visibility_base", &self.visibility_base)
            .field("caller", &self.caller)
            .field("clone_timeout", &self.clone_timeout)
            .field("refresh", &self.refresh)
            .field("unregistered", &self.unregistered)
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
            refresh: opts.refresh,
            fetches: &fetches,
        },
        opts.jobs,
        opts.visibility_base,
    );
    let probe_elapsed = start.elapsed();
    // after the fetches, never before: a session started while they ran holds
    let assessed = assess(
        entries,
        probes,
        &Assess {
            root,
            live: &(opts.read_live)(),
            caller: opts.caller,
            refresh: opts.refresh,
            unregistered: opts.unregistered.unwrap_or_default(),
        },
    );

    let start = Instant::now();
    let actor = Actor {
        git,
        root,
        entries,
        checkouts: &assessed.checkouts,
        read_live: opts.read_live,
        caller: opts.caller,
    };
    let cloner = Cloner {
        git,
        root,
        registry_dirs,
        timeout: opts.clone_timeout,
    };
    // the clones first: the longest tasks, each its own entry's
    let clones: Vec<(usize, &CloneRecipe)> = assessed
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.clone {
            Some(CloneVerdict::Act { recipe }) => Some((i, recipe)),
            _ => None,
        })
        .collect();
    let groups = repo_groups(&assessed.facts);
    let acted = run_pool(clones.len() + groups.len(), opts.jobs, |task| {
        if let Some(&(i, recipe)) = clones.get(task) {
            let busy = |path: &Path| !sessions_under(&(opts.read_live)(), path).is_empty();
            return Acted::Clone(i, cloner.clone_entry(&entries[i], recipe, busy));
        }
        let group = &groups[task - clones.len()];
        Acted::Repo(actor.act_on_repo(group, &assessed.entries, &assessed.facts))
    });
    let mut branches: Vec<Vec<BranchSync>> = vec![Vec::new(); entries.len()];
    let mut cloned: Vec<Option<CloneOutcome>> = vec![None; entries.len()];
    for acted in acted {
        match acted {
            Acted::Clone(i, outcome) => cloned[i] = Some(outcome),
            Acted::Repo(outcomes) => {
                for (i, outcomes) in outcomes {
                    branches[i] = outcomes;
                }
            }
        }
    }
    let outcomes = assessed
        .entries
        .iter()
        .zip(&assessed.fetches)
        .zip(branches.into_iter().zip(cloned))
        .map(|((e, fetch), (branches, cloned))| EntrySync {
            key: e.key.clone(),
            fetch: fetch_outcome(fetch.as_ref()),
            clone: e.clone.as_ref().map(|verdict| match verdict {
                CloneVerdict::Held { by, .. } => CloneOutcome::Held { by: (*by).into() },
                // never a guess that it was cloned
                CloneVerdict::Act { .. } => cloned.unwrap_or_else(|| CloneOutcome::Failed {
                    message: "sync didn't carry out the clone".to_owned(),
                }),
            }),
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

/// A pool task's outcomes: a clone's, by entry, or a repo's entries'.
enum Acted {
    Clone(usize, CloneOutcome),
    Repo(Vec<(usize, Vec<BranchSync>)>),
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
    /// The entries acted on, in the order the statuses are.
    entries: &'a [Entry],
    checkouts: &'a [EntryCheckouts],
    read_live: &'a (dyn Fn() -> LiveSessions + Sync),
    caller: Caller,
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
                        Verdict::Act { action } => {
                            match (stopped.get(b.name.as_str()), done.get(b.name.as_str())) {
                                (Some((by, o)), _) | (None, Some((by, o))) => {
                                    (o.clone(), Some(statuses[*by].key.clone()))
                                }
                                (None, None) => {
                                    let o = self.act(i, f, b, *action);
                                    done.insert(b.name.as_str(), (i, o.clone()));
                                    (o, None)
                                }
                            }
                        }
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
        // classify held it already, so no verdict reaches here as an agent's
        // push; a second line, before anything is read, should that slip
        // (`an_agents_push_is_held_at_act_time_whatever_the_verdict`)
        if matches!(action, SyncAction::Push { .. }) && self.caller == Caller::Agent {
            return held(SyncHold::Gateway);
        }
        // classify never makes a third-party reference's verdict a push (it
        // reads local-only); a second line, as above
        // (`a_third_party_push_fails_at_act_time_whatever_the_verdict`)
        if matches!(action, SyncAction::Push { .. }) && !self.entries[i].writable {
            return failed(format!(
                "{} is a third-party reference's, which is never pushed",
                b.name
            ));
        }
        let Some(branch) = facts.branches.iter().find(|f| f.branch.name == b.name) else {
            return failed(format!("{} isn't among the branches probed", b.name));
        };
        let Some(upstream) = branch.branch.upstream_ref.as_deref() else {
            return failed(format!("{} has no upstream to move to", b.name));
        };
        // the probed checkouts on it, from the facts classify read: it held
        // a fast-forward or move on several, or on an unprobed one; a push
        // on several goes on, since it moves no files
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
        if let Some(by) = self.busy_now(i, &b.name, &on) {
            return held(by);
        }
        // a partial clone lacks the new tip's blobs its checkout needs:
        // fetched on demand from origin alone, over origin's transport
        let lazy = lazy_fetch(&facts.config, self.git.env_configures_ssh());
        let step = Step::new(
            self.git,
            self.root,
            &b.name,
            upstream,
            &facts.common_dir,
            lazy,
        );
        let result = if let SyncAction::Push { commits } = action {
            let Some(target) = push_target(&branch.branch) else {
                // classify leaves it to a person; never a guess at a ref
                return failed(format!("{}'s upstream isn't a branch on origin", b.name));
            };
            step.push(
                Path::new(&facts.path),
                &Push {
                    oid: &branch.branch.oid,
                    target,
                    commits,
                    shallow: facts.layout.shallow,
                    url: &self.entries[i].url,
                    batch_ssh: !facts.config.ssh_command && !self.git.env_configures_ssh(),
                },
            )
        } else {
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
            match (action, checkout) {
                (SyncAction::Move, None) => step.move_in_place(Path::new(&facts.path)),
                (SyncAction::Move, Some(c)) => step.move_in_checkout(Path::new(c)),
                (_, None) => step.ff_in_place(Path::new(&facts.path)),
                (_, Some(c)) => step.ff_in_checkout(Path::new(c)),
            }
        };
        match result {
            Ok(Done::Updated { from, to }) if matches!(action, SyncAction::Move) => {
                BranchOutcome::Moved { from, to }
            }
            Ok(Done::Updated { from, to }) => BranchOutcome::FastForwarded { from, to },
            Ok(Done::Pushed { from, to }) => BranchOutcome::Pushed { from, to },
            Ok(Done::PushFailed(failure)) => BranchOutcome::PushFailed { failure },
            Ok(Done::AlreadyThere) => BranchOutcome::Untouched,
            Ok(Done::Held(by)) => held(by),
            Err(message) => failed(message),
        }
    }

    /// What holds an action on `branch` now, from the live sessions re-read:
    /// detection unavailable, or a session that may be on the branch
    /// through a git dir no worktree list names, holds any action; one in a
    /// checkout it's on (`checkouts`), or a checkout whose path can't be
    /// resolved, holds that checkout's.
    fn busy_now(&self, i: usize, branch: &str, checkouts: &[&str]) -> Option<SyncHold> {
        let live = (self.read_live)();
        let (_, per_entry) = scope_sessions(&live, self.checkouts);
        let sessions = &per_entry[i];
        if sessions.detection == Detection::Unavailable || sessions.unlisted_on(branch) > 0 {
            return Some(SyncHold::BusyUnknown);
        }
        if checkouts.iter().any(|c| !sessions.at(c).is_empty()) {
            Some(SyncHold::Busy)
        } else if checkouts.iter().any(|c| sessions.unresolved_at(c)) {
            Some(SyncHold::BusyUnknown)
        } else {
            None
        }
    }
}

/// A verdict that doesn't act, as an outcome. An `act` is `act_on_repo`'s
/// to carry out, never settled: reaching here would be a bug, reported as a
/// failure rather than passed over.
fn settled(verdict: &Verdict) -> BranchOutcome {
    match verdict {
        Verdict::Quiet | Verdict::LocalOnly | Verdict::Cleanup { .. } => BranchOutcome::Untouched,
        Verdict::NeedsHuman { reason } => BranchOutcome::NeedsHuman { reason: *reason },
        Verdict::Held { action, by } => BranchOutcome::Held {
            action: *action,
            by: (*by).into(),
        },
        Verdict::Act { action } => BranchOutcome::Failed {
            action: *action,
            message: "sync didn't carry out the verdict".to_owned(),
        },
    }
}

/// How an action went, short of failing.
#[derive(Debug)]
enum Done {
    /// The branch moved from `from` to `to`.
    Updated { from: String, to: String },
    /// The remote's branch moved from `from` (`None`: it had none) to `to`.
    Pushed { from: Option<String>, to: String },
    /// The push failed at the remote, or reaching it.
    PushFailed(RemoteFailure),
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
    /// A partial clone's lazy fetch, for the actions that rewrite a working
    /// tree (`run_checkout`); `None` keeps it off.
    lazy: Option<LazyFetch>,
}

/// How an action that rewrites a partial clone's working tree fetches the
/// objects it lacks: from its promisor remote, origin — only when no other
/// remote is one (`ConfigFacts::other_promisor`), since git asks each in
/// turn — over `transport` alone (`lazy_transport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LazyFetch {
    transport: &'static str,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    batch_ssh: bool,
}

/// A partial clone's lazy fetch, from its `config`: `None` — lazy fetching
/// stays off, and a checkout needing a missing blob fails — for a repo
/// that isn't a partial clone, one with another promisor remote, and one
/// whose origin URL names no transport the fetch may take
/// (`lazy_transport`). `env_ssh`: the environment configures SSH.
///
/// Only when classify let the branch act, so with origin naming the
/// registry's repo (origin drift holds the entry, and a reference's
/// refresh).
fn lazy_fetch(config: &ConfigFacts, env_ssh: bool) -> Option<LazyFetch> {
    if config.partial_filter.is_none() || config.other_promisor {
        return None;
    }
    Some(LazyFetch {
        transport: lazy_transport(config.origin_url()?)?,
        batch_ssh: !config.ssh_command && !env_ssh,
    })
}

/// The one transport a lazy fetch from `origin` may take: its own — `ssh`
/// for an SSH URL (`ssh://`, its `git+ssh` spellings, or scp-like),
/// `https` for an HTTPS one — whoever owns the repo, so an owned partial
/// clone whose origin is HTTPS fills its checkout over HTTPS. `None` for
/// anything else (plain `http`, `git://`, a local path, a URL
/// `remote_parts` rejects): no lazy fetch.
fn lazy_transport(origin: &str) -> Option<&'static str> {
    let parts = remote_parts(origin)?;
    if parts.ssh {
        Some("ssh")
    } else if origin.starts_with("https://") {
        Some("https")
    } else {
        None
    }
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

/// What a push sends, as classified.
struct Push<'a> {
    /// The commit the branch held when probed: the one pushed, whatever
    /// lands on the branch after.
    oid: &'a str,
    /// The upstream's ref on origin (`push_target`), named explicitly.
    target: &'a str,
    /// The commits ahead the verdict counted.
    commits: u32,
    /// A shallow clone, where the verdict counted commits on no remote ref.
    shallow: bool,
    /// The registry's URL, which the push URL must name.
    url: &'a RepoUrl,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    batch_ssh: bool,
}

/// The push's flags before `origin` and its refspec. No force of any kind,
/// and nothing but the one ref: no tags (`push.followTags`), no submodule
/// pushes (`push.recurseSubmodules`), no push certificate
/// (`push.gpgSign` — a signing prompt under a batch run, and a host that
/// takes none fails the push), and no push options (`push.pushOption`,
/// which the empty value resets: a host can act on one, and one it doesn't
/// take fails the push). `--receive-pack` names git's own remote command
/// over `remote.origin.receivepack`, which could otherwise run another
/// command on the host, or point the push at another path there. Hooks are
/// off in the runner already;
/// `--no-verify` says so again. `core.abbrev=no` makes the porcelain's
/// summary full object ids. An explicit refspec also passes over
/// `remote.origin.push` and `push.default`; a `remote.origin.mirror`
/// makes git refuse it outright.
const PUSH_ARGS: [&str; 11] = [
    "-c",
    "core.abbrev=no",
    "-c",
    "push.pushOption=",
    "push",
    "--porcelain",
    "--receive-pack=git-receive-pack",
    "--no-verify",
    "--no-follow-tags",
    "--recurse-submodules=no",
    "--no-signed",
];

impl<'a> Step<'a> {
    /// A step whose calls stop repo discovery at `root` and are local:
    /// lazy fetching off in every one but `run_checkout`'s, which lifts it
    /// by `lazy`.
    fn new(
        git: &'a Git,
        root: &'a Path,
        branch: &'a str,
        upstream: &'a str,
        common_dir: &'a Path,
        lazy: Option<LazyFetch>,
    ) -> Self {
        Self {
            git,
            opts: CallOptions {
                ceiling: Some(root),
                ..CallOptions::default()
            },
            branch,
            upstream,
            common_dir,
            lazy,
        }
    }
}

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
        Ok(self.count_local_work(dir, commit)? > 0)
    }

    /// `commit`'s commits on no remote-tracking ref, shallow roots aside.
    fn count_local_work(&self, dir: &Path, commit: &str) -> Result<usize, String> {
        let out = self
            .git
            .output_string(dir, &["rev-list", commit, "--not", "--remotes"], self.opts)
            .map_err(|e| git_message(&e))?;
        let roots = read_shallow_roots(self.common_dir);
        Ok(out.lines().filter(|c| !roots.contains(*c)).count())
    }

    /// Pushes `p.oid` through `origin` to `p.target`, once the branch reads
    /// as classified — the same commit, upstream, and target, no symbolic
    /// ref — the push URL still names the registry's repo over SSH, and the
    /// commit is still the counted commits ahead of the remote-tracking ref.
    /// Git's own refusal of anything but a fast-forward of what the remote
    /// holds is the last check.
    fn push(&self, dir: &Path, p: &Push<'_>) -> Result<Done, String> {
        if !self.reads_as_classified(dir, p)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        if !push_urls_match(&read_push_urls(self.git, dir, self.opts)?, p.url) {
            return Ok(Done::Held(SyncHold::PushUrl));
        }
        let fetched = self.resolve(dir, self.upstream)?;
        if fetched == p.oid {
            return Ok(Done::AlreadyThere);
        }
        let ahead = if p.shallow {
            self.count_local_work(dir, p.oid)?
        } else {
            let range = format!("{fetched}..{}", p.oid);
            self.git
                .output_string(dir, &["rev-list", "--count", &range], self.opts)
                .map_err(|e| git_message(&e))?
                .trim()
                .parse()
                .map_err(|_| format!("rev-list --count {range}: not a count"))?
        };
        if !self.is_ancestor(dir, &fetched, p.oid)? || ahead != p.commits as usize {
            return Ok(Done::Held(SyncHold::Changed));
        }
        let refspec = format!("{}:{}", p.oid, p.target);
        let mut args = PUSH_ARGS.to_vec();
        args.extend(["origin", refspec.as_str()]);
        let opts = CallOptions {
            network: Some(NetworkOptions {
                batch_ssh: p.batch_ssh,
            }),
            // the push URL was just read as SSH: a config changed since
            // can't send the push over another transport
            allow_protocol: Some("ssh"),
            ..self.opts
        };
        let out = match self.git.run(dir, &args, opts) {
            Ok(out) => out,
            Err(e) => {
                return Ok(Done::PushFailed(RemoteFailure::from_git_error(
                    e,
                    RefspecContext::default(),
                )));
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        match pushed_ref(&stdout, p.target) {
            Some(PushedRef { flag: ' ', summary }) => match summary.split_once("..") {
                Some((from, to)) if to == p.oid => Ok(Done::Pushed {
                    from: Some(from.to_owned()),
                    to: to.to_owned(),
                }),
                _ => Err(format!("git pushed {}: {summary}", p.target)),
            },
            Some(PushedRef { flag: '*', .. }) => Ok(Done::Pushed {
                from: None,
                to: p.oid.to_owned(),
            }),
            Some(PushedRef { flag: '=', .. }) => Ok(Done::AlreadyThere),
            Some(PushedRef { flag: '!', summary }) => Ok(rejected(summary, &out.stderr)),
            Some(PushedRef { flag, summary }) => Err(format!(
                "git reported `{flag}` pushing {}: {summary}",
                p.target
            )),
            None if out.status.success() => {
                Err(format!("git push reported nothing for {}", p.target))
            }
            None => Ok(Done::PushFailed(RemoteFailure::from_git_error(
                GitError::Failed {
                    args: args.join(" "),
                    code: out.status.code(),
                    stderr: out.stderr,
                },
                RefspecContext::default(),
            ))),
        }
    }

    /// Whether the branch in `dir` is as classified: the commit `p.oid`, a
    /// plain ref, its upstream the same remote-tracking ref, on `origin`,
    /// at `p.target` there. `false` when deleted since.
    fn reads_as_classified(&self, dir: &Path, p: &Push<'_>) -> Result<bool, String> {
        let local = self.local();
        let out = self
            .git
            .output_string(
                dir,
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(symref)%00%(upstream)%00\
                     %(upstream:remotename)%00%(upstream:remoteref)",
                    &local,
                ],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        // the pattern also matches refs under it (`<b>/x`): only the ref
        let expected = [local.as_str(), p.oid, "", self.upstream, "origin", p.target];
        Ok(out
            .lines()
            .map(|l| l.split('\0').collect::<Vec<_>>())
            .find(|f| f.first() == Some(&local.as_str()))
            .is_some_and(|f| f == expected))
    }

    /// Runs git in `checkout` to rewrite its working tree, under
    /// `CHECKOUT_TIMEOUT` — in a partial clone, with its lazy fetch
    /// (`LazyFetch`): the new tip's blobs in the checkout's cone may never
    /// have been fetched, and git reads them from the promisor remote.
    fn run_checkout(&self, checkout: &Path, args: &[&str]) -> Result<(), String> {
        let mut opts = CallOptions {
            timeout: Some(CHECKOUT_TIMEOUT),
            ..self.opts
        };
        if let Some(lazy) = self.lazy {
            opts.lazy_fetch = true;
            opts.allow_protocol = Some(lazy.transport);
            opts.network = Some(NetworkOptions {
                batch_ssh: lazy.batch_ssh,
            });
        }
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

/// One ref's line in `git push --porcelain`'s output, tab-separated:
/// `<flag>`, `<src>:<dst>`, `<summary>`.
#[derive(Debug, PartialEq, Eq)]
struct PushedRef<'a> {
    /// ` ` a fast-forward, `*` a new ref, `=` up to date, `!` rejected, `+`
    /// forced, `-` deleted.
    flag: char,
    /// `<old>..<new>` for a fast-forward, else git's bracketed status and
    /// its reason, e.g. `[remote rejected] (pre-receive hook declined)`.
    summary: &'a str,
}

/// The porcelain line for the push to `dst`, if git printed one.
fn pushed_ref<'a>(stdout: &'a str, dst: &str) -> Option<PushedRef<'a>> {
    stdout.lines().find_map(|l| {
        let mut f = l.splitn(3, '\t');
        let (flag, refs, summary) = (f.next()?, f.next()?, f.next()?);
        let mut chars = flag.chars();
        let flag = chars.next().filter(|_| chars.next().is_none())?;
        (refs.rsplit_once(':')?.1 == dst).then_some(PushedRef { flag, summary })
    })
}

/// A push git rejected (`!`), by its summary: the remote moved since the
/// fetch (`fetch first`, `non-fast-forward` — git refuses to overwrite it)
/// is held, `Changed`, for a rerun to reclassify; the remote's own refusal
/// (`[remote rejected]`: a ruleset, a hook) fails with its reason and the
/// remote's first error line; anything else fails with git's summary.
fn rejected(summary: &str, stderr: &str) -> Done {
    let reason = |status: &str| {
        summary
            .strip_prefix(status)
            .map(|r| r.trim().trim_start_matches('(').trim_end_matches(')'))
    };
    match reason("[rejected]") {
        Some("fetch first" | "non-fast-forward") => return Done::Held(SyncHold::Changed),
        Some(_) => {
            return Done::PushFailed(RemoteFailure::Failed {
                message: format!("rejected: {summary}"),
            });
        }
        None => {}
    }
    if let Some(reason) = reason("[remote rejected]") {
        return Done::PushFailed(RemoteFailure::Rejected {
            reason: reason.to_owned(),
            message: remote_error(stderr),
        });
    }
    Done::PushFailed(RemoteFailure::Failed {
        message: summary.to_owned(),
    })
}

/// The remote's first `error:` line, as git relays it (`remote: error: …`,
/// padded), else its first line; `None` when it sent none.
fn remote_error(stderr: &str) -> Option<String> {
    let remote = || {
        stderr
            .lines()
            .filter_map(|l| l.strip_prefix("remote:"))
            .map(str::trim)
            .filter(|l| !l.is_empty())
    };
    remote()
        .find_map(|l| l.strip_prefix("error:").map(str::trim))
        .or_else(|| remote().next())
        .map(str::to_owned)
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

    #[test]
    fn a_push_is_read_from_its_porcelain_line() {
        let dst = "refs/heads/main";
        let out = "To github.com:me/app\n \tc2:refs/heads/main\tc1..c2\nDone\n";
        assert_eq!(
            pushed_ref(out, dst),
            Some(PushedRef {
                flag: ' ',
                summary: "c1..c2"
            })
        );
        // another ref's line, or none, is no answer
        assert_eq!(pushed_ref(out, "refs/heads/mai"), None);
        assert_eq!(pushed_ref("To x\nDone\n", dst), None);
        let rejected_line = "!\tc2:refs/heads/main\t[rejected] (fetch first)\n";
        assert_eq!(pushed_ref(rejected_line, dst).map(|p| p.flag), Some('!'));
    }

    #[test]
    fn a_rejected_push_is_held_or_failed_by_why() {
        // the remote moved: rerun
        for why in ["fetch first", "non-fast-forward"] {
            assert!(matches!(
                rejected(&format!("[rejected] ({why})"), ""),
                Done::Held(SyncHold::Changed)
            ));
        }
        // the remote's refusal, with its own words
        let stderr = "remote: error: GH006: Protected branch update failed for refs/heads/main.   \n\
                      remote: error: Changes must be made through a pull request.   \n\
                      error: failed to push some refs to 'github.com:me/app'\n";
        assert!(matches!(
            rejected("[remote rejected] (protected branch hook declined)", stderr),
            Done::PushFailed(RemoteFailure::Rejected { reason, message })
                if reason == "protected branch hook declined"
                    && message.as_deref()
                        == Some("GH006: Protected branch update failed for refs/heads/main.")
        ));
        assert!(matches!(
            rejected("[remote rejected] (hook declined)", "remote: nope\n"),
            Done::PushFailed(RemoteFailure::Rejected { message: Some(m), .. }) if m == "nope"
        ));
        assert!(matches!(
            rejected("[remote rejected] (hook declined)", ""),
            Done::PushFailed(RemoteFailure::Rejected { message: None, .. })
        ));
        // anything else git refused fails with its words
        assert!(matches!(
            rejected("[rejected] (stale info)", ""),
            Done::PushFailed(RemoteFailure::Failed { message })
                if message == "rejected: [rejected] (stale info)"
        ));
    }

    /// `main` of `/ws/app`, checked out and a commit ahead, as classify
    /// would read it, its verdict a push; no branch probed, so past the
    /// guards the push would fail on it.
    fn ahead_main() -> (RepoFacts, BranchStatus) {
        let facts = RepoFacts {
            path: "/ws/app".into(),
            git_dir: PathBuf::from("/ws/app/.git"),
            common_dir: PathBuf::from("/ws/app/.git"),
            config: ConfigFacts::default(),
            status: porcelain::StatusFacts {
                head: Head::Branch {
                    name: "main".into(),
                },
                uncommitted: crate::state::Uncommitted::default(),
                stashes: 0,
            },
            in_progress: None,
            primary_linked: false,
            primary_locked: false,
            locks: Vec::new(),
            worktrees: Vec::new(),
            registry_worktrees: std::collections::HashSet::new(),
            unprobed: Vec::new(),
            unreadable: Vec::new(),
            relative_gitdir: None,
            git_dirs: Vec::new(),
            bare_main: None,
            branches: Vec::new(),
            layout: crate::state::Layout {
                shallow: false,
                sparse: false,
                partial_filter: None,
            },
            fetched_at: None,
            fetch_failed: false,
            push_urls: Some(vec!["git@github.com:me/app".into()]),
        };
        let b = BranchStatus {
            name: "main".into(),
            upstream: Some("origin/main".into()),
            worktree: Some("/ws/app".into()),
            symref: None,
            unique_commits: 1,
            newest_commit_at: 0,
            relation: crate::state::Relation::Ahead { commits: 1 },
            verdict: Verdict::Act {
                action: SyncAction::Push { commits: 1 },
            },
        };
        (facts, b)
    }

    /// The act-time gateway hold, driven directly: classify holds an
    /// agent's push before it becomes an `act` (so no run through `sync`
    /// reaches this guard), and the guard is the second line should that
    /// ever slip. It holds before anything is read — no facts, no sessions,
    /// no git.
    #[test]
    fn an_agents_push_is_held_at_act_time_whatever_the_verdict() {
        // a runner with no `PATH`: git can't even start
        let git = Git::with_clean_env(Vec::new());
        let read_live = || -> LiveSessions { panic!("the guard reads no sessions") };
        let actor = Actor {
            git: &git,
            root: Path::new("/ws"),
            entries: &[],
            checkouts: &[],
            read_live: &read_live,
            caller: Caller::Agent,
        };
        let (facts, b) = ahead_main();
        let action = SyncAction::Push { commits: 1 };
        assert_eq!(
            actor.act(0, &facts, &b, action),
            BranchOutcome::Held {
                action,
                by: SyncHold::Gateway
            }
        );
    }

    /// The act-time third-party guard, driven directly as the gateway's is:
    /// classify reads a third-party reference's branch ahead as local-only
    /// work, never a push, and the guard fails one that ever slips, before
    /// anything is read.
    #[test]
    fn a_third_party_push_fails_at_act_time_whatever_the_verdict() {
        let git = Git::with_clean_env(Vec::new());
        let read_live = || -> LiveSessions { panic!("the guard reads no sessions") };
        let lib = Entry {
            key: "lib".into(),
            kind: crate::registry::EntryKind::Reference,
            dir: "lib".into(),
            url: RepoUrl::try_from("https://github.com/them/lib".to_owned()).unwrap(),
            writable: false,
            archived: false,
            visibility: None,
            ci: false,
            branch: None,
            pinned: false,
            shallow: false,
            sparse: None,
            same_repo_as: None,
        };
        let entries = [lib];
        let actor = Actor {
            git: &git,
            root: Path::new("/ws"),
            entries: &entries,
            checkouts: &[],
            read_live: &read_live,
            caller: Caller::Person,
        };
        let (facts, b) = ahead_main();
        let action = SyncAction::Push { commits: 1 };
        assert_eq!(
            actor.act(0, &facts, &b, action),
            BranchOutcome::Failed {
                action,
                message: "main is a third-party reference's, which is never pushed".into(),
            }
        );
    }

    // the checks after the fact, driven from just past the re-checks: the
    // races they catch fall between a re-check and git's write, which no
    // seam in `sync` reaches

    /// A repo in a tempdir, no global or system config, reflogs off — so
    /// what a branch's reflog holds, sync wrote.
    struct Repo {
        tmp: tempfile::TempDir,
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
            let repo = Self { tmp, dir, env };
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
        Step::new(
            git,
            Path::new("/"),
            branch,
            "refs/remotes/origin/unused",
            common_dir,
            None,
        )
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

    // the lazy fetch's scope: one transport, origin's own, and the
    // checkout's calls alone

    #[test]
    fn a_lazy_fetch_takes_origins_own_transport_alone() {
        for (origin, want) in [
            ("git@github.com:me/wpt", Some("ssh")),
            ("ssh://git@github.com/me/wpt", Some("ssh")),
            ("git+ssh://github.com/me/wpt", Some("ssh")),
            ("https://github.com/them/lib", Some("https")),
            // an owned repo whose origin is HTTPS: over HTTPS, never SSH
            ("https://github.com/me/wpt", Some("https")),
            ("http://github.com/them/lib", None),
            ("git://github.com/them/lib", None),
            ("file:///srv/lib.git", None),
            ("/srv/lib.git", None),
            ("ext::sh -c touch% /tmp/x", None),
            ("https://github.com/them/%6Cib", None),
        ] {
            assert_eq!(lazy_transport(origin), want, "{origin}");
        }
    }

    #[test]
    fn a_lazy_fetch_is_a_partial_clones_with_origin_its_one_promisor() {
        let partial = |origin: Option<&str>| ConfigFacts {
            origin_urls: origin.map(porcelain::OriginUrl::repo).into_iter().collect(),
            partial_filter: Some("blob:none".into()),
            ..ConfigFacts::default()
        };
        let https = partial(Some("https://github.com/me/wpt"));
        assert_eq!(
            lazy_fetch(&https, false),
            Some(LazyFetch {
                transport: "https",
                batch_ssh: true,
            })
        );
        let ssh = partial(Some("git@github.com:them/lib"));
        assert_eq!(
            lazy_fetch(&ssh, false),
            Some(LazyFetch {
                transport: "ssh",
                batch_ssh: true,
            })
        );
        // the user's SSH, left alone
        assert_eq!(lazy_fetch(&ssh, true).map(|l| l.batch_ssh), Some(false));
        let configured = ConfigFacts {
            ssh_command: true,
            ..ssh.clone()
        };
        assert_eq!(
            lazy_fetch(&configured, false).map(|l| l.batch_ssh),
            Some(false)
        );
        // none: not partial, another promisor, no origin URL, or one naming
        // no transport the fetch may take
        let whole = ConfigFacts {
            partial_filter: None,
            ..ssh.clone()
        };
        let other = ConfigFacts {
            other_promisor: true,
            ..ssh
        };
        for config in [
            whole,
            other,
            partial(None),
            partial(Some("file:///srv/wpt.git")),
        ] {
            assert_eq!(lazy_fetch(&config, false), None, "{config:?}");
        }
    }

    /// Every action's git calls, through a `git` that logs what each saw:
    /// lazy fetching is lifted — over the lazy fetch's one transport — in
    /// the calls that rewrite a working tree (`merge`, `switch`) and no
    /// other, however the step was made.
    #[test]
    fn only_a_checkouts_calls_lift_lazy_fetching() {
        let repo = Repo::new();
        let root = repo.git(&["rev-parse", "main"]);
        let tip = repo.child_of(&root);
        let side = repo.child_of(&tip);
        for (name, at) in [
            ("main", &tip),
            ("feat", &tip),
            ("old", &side),
            ("side", &side),
        ] {
            repo.git(&["update-ref", &format!("refs/remotes/origin/{name}"), at]);
        }
        repo.git(&["branch", "feat", &root]);
        repo.git(&["branch", "old", &root]);
        // a `git` first on PATH, logging each call it passes to the real one
        let bin = repo.tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let log = repo.tmp.path().join("calls.log");
        let real_path = std::env::var("PATH").unwrap();
        let wrapper = format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"${{GIT_NO_LAZY_FETCH-unset}}\" \
             \"${{GIT_ALLOW_PROTOCOL-unset}}\" \"$*\" >> '{}'\nPATH='{real_path}' exec git \"$@\"\n",
            log.display()
        );
        std::fs::write(bin.join("git"), wrapper).unwrap();
        std::fs::set_permissions(
            bin.join("git"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let mut env = repo.env.clone();
        env.retain(|(k, _)| k != "PATH");
        env.push((
            "PATH".into(),
            format!("{}:{real_path}", bin.display()).into(),
        ));
        let git = Git::with_clean_env(env);
        let common_dir = repo.dir.join(".git");
        let lazy = Some(LazyFetch {
            transport: "https",
            batch_ssh: false,
        });
        let step = |branch, upstream| {
            Step::new(&git, repo.tmp.path(), branch, upstream, &common_dir, lazy)
        };

        let updated = |done: Result<Done, String>| matches!(done, Ok(Done::Updated { .. }));
        assert!(updated(
            step("feat", "refs/remotes/origin/feat").ff_in_place(&repo.dir)
        ));
        assert!(updated(
            step("main", "refs/remotes/origin/main").ff_in_checkout(&repo.dir)
        ));
        assert!(updated(
            step("old", "refs/remotes/origin/old").move_in_place(&repo.dir)
        ));
        let moved = step("main", "refs/remotes/origin/side").move_in_checkout(&repo.dir);
        assert!(matches!(moved, Ok(Done::Updated { .. })), "{moved:?}");
        let url = RepoUrl::try_from("https://github.com/me/app".to_owned()).unwrap();
        let oid = repo.git(&["rev-parse", "main"]);
        // held at its re-checks, past its reads
        let pushed = step("main", "refs/remotes/origin/side").push(
            &repo.dir,
            &Push {
                oid: &oid,
                target: "refs/heads/main",
                commits: 1,
                shallow: false,
                url: &url,
                batch_ssh: false,
            },
        );
        assert!(matches!(pushed, Ok(Done::Held(_))), "{pushed:?}");

        let calls = std::fs::read_to_string(&log).unwrap();
        let mut checkouts = 0;
        for call in calls.lines() {
            let mut f = call.splitn(3, '|');
            let (lazy, allowed, args) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
            let checkout = [" merge ", " switch "].iter().any(|c| args.contains(c));
            if checkout {
                checkouts += 1;
                assert_eq!((lazy, allowed), ("0", "https"), "{call}");
            } else {
                assert_eq!(lazy, "1", "{call}");
                assert_ne!(allowed, "https", "{call}");
            }
        }
        assert_eq!(checkouts, 2, "{calls}");
    }
}
