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
//! **Never** a force-push, a tag pushed, a remote branch created (the
//! user's `repos push --new-branch` alone creates one), a rebase,
//! a merge that isn't a fast-forward, a clone over anything at an entry's
//! path, a deleted branch, or a pruned worktree (the origin fetch's
//! `--prune` deletes only remote-tracking refs gone upstream); a pin, once
//! there, is never touched (its verdicts never act), and a third-party
//! reference only when the run refreshes it — named as a target, or under
//! `--references` — and then never pushed. An entry whose probe failed has
//! no verdicts, so nothing in it acts. An agent's run pushes as a person's
//! does — every branch ahead that nothing holds, busy detection keeping it
//! off live sessions' checkouts — and `repos push` (the `push` module)
//! pushes one checkout's branch through this module's one push
//! (`Actor::push`).
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
//! - **A push** is `git send-pack` of the commit the branch held when
//!   probed to its upstream's ref on origin (`push_target`: a branch, never
//!   `refs/heads/HEAD`), so a commit landing after classifying is never
//!   pushed unseen — sent to the registry's URL over SSH as written, never
//!   through `origin` (`SEND_PACK_ARGS` says why: no rewrite or remote
//!   config reaches it). Right before, sync re-reads the branch — the same
//!   commit, upstream, and ref on origin, no symbolic ref, else `changed` —
//!   re-reads where a push through origin would go (`git remote get-url
//!   --push --all`, `pushurl` and `pushInsteadOf` applied: exactly one URL,
//!   the registry's repo over SSH as `push_urls_match` reads it — the host
//!   git connects to and the path there, never the URL's text — else held
//!   `push_url`: origin pushing elsewhere is a person's to sort out, even
//!   though the push itself never reads it), and re-counts the commits
//!   ahead of the remote-tracking ref (the same count, the ref an ancestor,
//!   else `changed`). The push is under a lease on that fetched tip
//!   (`--force-with-lease=<ref>:<fetched>`), a compare-and-swap: git
//!   refuses unless the remote's branch is exactly what the fetch saw, and
//!   the remote updates it only from the value it advertised. A lease lifts
//!   git's own fast-forward check, so the ancestor re-check is what keeps
//!   it one: together, a strict fast-forward of exactly the fetched tip,
//!   never a force over work the fetch didn't see (a host refusing
//!   non-fast-forwards checks it again). A remote branch moved since the
//!   fetch — forward, back, or deleted, and deleted and recreated anywhere
//!   but the fetched tip — fails the lease and is held (`changed`) for a
//!   rerun, which reclassifies it: a deleted branch reads `gone`, so no
//!   push recreates one (the user's `repos push --new-branch` alone does,
//!   and only while it has commits on no remote). The push sends nothing but the one ref — no
//!   tags, no push options, no push certificate — with git's own remote
//!   command, over SSH only (`GIT_ALLOW_PROTOCOL=ssh`), batch-mode as the
//!   fetch. The remote's own refusal (a ruleset, a hook) or a host
//!   unreachable fails (`push_failed`, classified). Once pushed — or
//!   found there already, another hand's push of the very commit since the
//!   fetch — the remote-tracking ref moves to the commit by compare-and-swap
//!   on the fetched tip (`record_push`), so `status` reads the branch in
//!   sync without a refetch; a fetch that moved it meanwhile wins. A failed
//!   or refused fetch holds every push, so that ref is one the fetch
//!   confined.
//!
//! - **A new remote branch** is `repos push --new-branch`'s alone (sync
//!   never creates one): the same send-pack to the registry's URL, of the
//!   commit classified to `refs/heads/<b>` under the branch's own name,
//!   under a lease that no such ref exists (`--force-with-lease=<ref>:`),
//!   so a branch created there since the fetch is never overwritten
//!   (`changed`). Right before, the same re-checks as a push's, and the
//!   remote-tracking ref origin's fetch refspec maps it to: none holds it
//!   for a person, and one the fetch wrote, at another commit, is a branch
//!   origin has, never adopted. Then, as `git push -u`, that ref by
//!   compare-and-swap on none, and the upstream config (`Step::create`
//!   says what a run stopped partway leaves, and how the next finishes it).
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
//! alone over the transport its URL names as git resolves it — the URL the
//! fetch connects to, `insteadOf` applied (`lazy_transport`) — writing
//! objects and no ref; one resolving to neither SSH nor HTTPS is a
//! person's (`fetch_url_mismatch`). Right before the checkout, that URL is
//! read again, and a URL that no longer names the registry's repo over
//! that transport holds the action (`changed`), as does another promisor
//! remote configured since, which git would ask too. Every other call
//! keeps lazy fetching off.
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
//! fast-forward brings in is executed — the `reference-transaction` hook
//! included, and `send-pack` runs no `pre-push` hook at all. Programs the
//! local config names — filter drivers such as Git LFS's smudge, the gpg
//! program `merge.verifySignatures` calls, SSH as configured
//! (`core.sshCommand`) — are the user's own and run as in any merge,
//! checkout, or push they'd make.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::busy::{Detection, EntryCheckouts, scope_sessions, sessions_under};
use crate::classify::{Refresh, push_target};
use crate::clone::{CLONE_TIMEOUT, Cloner};
use crate::discover::{Locate, Workspace, resolve_targets};
use crate::error;
use crate::git::Git;
use crate::porcelain::RefFacts;
use crate::probe::RepoFacts;
use crate::registry::{Entry, RegistryDirs};
use crate::remote::RemoteFailure;
use crate::report::{
    BranchOutcome, BranchSync, CloneOutcome, EntryStatus, EntrySync, FetchOutcome, PushOutcome,
    Sessions, SyncHold, SyncReport, UnregisteredClone,
};
use crate::sessions::{LiveSessions, SessionsSource, read_live_sessions};
use crate::state::{
    BranchNeedsHuman, BranchStatus, CloneRecipe, CloneVerdict, Head, SyncAction, Verdict,
};
use crate::status::{
    EntryTiming, Reported, RunTimings, Survey, assemble_report, probe_and_assess, refresh_asked,
    run_pool, scan_workspace,
};

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

/// How to make `repos sync`'s report (`sync_report`).
#[derive(Debug, Clone, Copy)]
pub struct SyncReportOptions<'a> {
    /// `--references`: refresh every third-party reference; only without
    /// targets.
    pub references: bool,
    /// Git calls in flight at once, at least one.
    pub jobs: usize,
    /// Where the live sessions are read (`SessionsSource::from_env`), after
    /// the fetches and again before each action.
    pub sessions: &'a SessionsSource,
}

/// `repos sync` over the entries `targets` name, and its report, the run's
/// policy included.
///
/// Loads the workspace (`Workspace::load`, from `cwd`), resolves `targets`
/// against it (`resolve_targets`: none is every entry), runs the
/// unregistered scan when the run needs it — before anything is cloned, so
/// a missing entry cloned under another name holds its clone — then
/// fetches, classifies, and acts (`sync`), each clone under
/// `CLONE_TIMEOUT`, and assembles the report as `status_report` does.
///
/// # Errors
///
/// As `status_report`'s: nothing has been fetched or acted on when it
/// fails. A failure in the run is the report's to say.
pub fn sync_report(
    git: &Git,
    cwd: &Path,
    locate: Locate<'_>,
    targets: &[String],
    opts: SyncReportOptions<'_>,
) -> error::Result<Reported<SyncReport>> {
    let start = Instant::now();
    let refresh = refresh_asked(targets, opts.references)?;
    let ws = Workspace::load(git, cwd, cwd, locate)?;
    let entries = resolve_targets(&ws.entries, ws.root(), cwd, targets, git)?;
    let load = start.elapsed();
    let scan_start = Instant::now();
    let scan = scan_workspace(&ws, &entries, targets, git)?;
    let scan_time = scan.as_ref().map(|_| scan_start.elapsed());

    let read_live = || read_live_sessions(opts.sessions);
    let run = sync(
        &entries,
        &ws.registry_dirs(),
        ws.root(),
        git,
        SyncOptions {
            jobs: opts.jobs,
            visibility_base: None,
            read_live: &read_live,
            clone_timeout: CLONE_TIMEOUT,
            refresh,
            unregistered: scan.as_ref().map(|s| &s.unregistered[..]),
        },
    );
    let status = assemble_report(
        &ws,
        true,
        run.sessions,
        run.entries,
        scan.filter(|_| targets.is_empty()),
    );
    Ok(Reported {
        report: SyncReport::new(status, run.outcomes),
        timings: RunTimings {
            load,
            scan: scan_time,
            probe: run.probe_elapsed,
            act: Some(run.act_elapsed),
            entries: run.timings,
        },
    })
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
    let (assessed, probe_elapsed) = probe_and_assess(
        entries,
        Survey {
            git,
            root,
            registry_dirs,
            fetch: true,
            refresh: opts.refresh,
            jobs: opts.jobs,
            visibility_base: opts.visibility_base,
            unregistered: opts.unregistered.unwrap_or_default(),
        },
        opts.read_live,
    );

    let start = Instant::now();
    let actor = Actor {
        git,
        root,
        entries,
        checkouts: &assessed.checkouts,
        read_live: opts.read_live,
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

/// An entry's fetch as an outcome: `None` when none was attempted.
pub(crate) fn fetch_outcome(fetch: Option<&Result<(), RemoteFailure>>) -> FetchOutcome {
    match fetch {
        None => FetchOutcome::NotFetched,
        Some(Ok(())) => FetchOutcome::Fetched,
        Some(Err(failure)) => FetchOutcome::Failed {
            failure: failure.clone(),
        },
    }
}

/// The entries with facts, grouped by the repo they share
/// (`RepoFacts::repo_key`), each group in entry order, the groups by their
/// first entry.
fn repo_groups(facts: &[Option<RepoFacts>]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut by_repo: HashMap<&Path, usize> = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        let Some(f) = f else { continue };
        let g = *by_repo.entry(&f.repo_key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    groups
}

/// What acting needs: the runner, the root discovery stops at, and every
/// entry's checkouts, which the live sessions re-read before each action
/// are scoped to. `repos push` acts through it too, on one branch's push.
pub(crate) struct Actor<'a> {
    pub git: &'a Git,
    pub root: &'a Path,
    /// The entries acted on, in the order the statuses are.
    pub entries: &'a [Entry],
    pub checkouts: &'a [EntryCheckouts],
    pub read_live: &'a (dyn Fn() -> LiveSessions + Sync),
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

    /// Takes `action` on branch `b` of entry `i`, re-checking first — the
    /// one way sync acts: a push through `push`, a fast-forward or move
    /// through `update`.
    fn act(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        action: SyncAction,
    ) -> BranchOutcome {
        let held = |by| BranchOutcome::Held { action, by };
        let failed = |message: String| BranchOutcome::Failed { action, message };
        let kind = match action {
            SyncAction::Push { commits } => {
                return match self.push(i, facts, b, commits) {
                    Ok(PushDone::Pushed { from, to }) => BranchOutcome::Pushed { from, to },
                    Ok(PushDone::AlreadyThere) => BranchOutcome::Untouched,
                    Ok(PushDone::Stopped(Stop::Held(by))) => held(by),
                    Ok(PushDone::Stopped(Stop::PushFailed(failure))) => {
                        BranchOutcome::PushFailed { failure }
                    }
                    Err(message) => failed(message),
                };
            }
            SyncAction::FastForward { .. } => UpdateKind::FastForward,
            SyncAction::Move => UpdateKind::Move,
        };
        match self.update(i, facts, b, kind) {
            Ok(UpdateDone::Updated { from, to }) => match kind {
                UpdateKind::FastForward => BranchOutcome::FastForwarded { from, to },
                UpdateKind::Move => BranchOutcome::Moved { from, to },
            },
            Ok(UpdateDone::AlreadyThere) => BranchOutcome::Untouched,
            Ok(UpdateDone::Held(by)) => held(by),
            Err(message) => failed(message),
        }
    }

    /// Pushes branch `b` of entry `i`, the `commits` ahead classify
    /// counted, re-checking first (`Step::push`) — the one push sync and
    /// `repos push` make.
    pub(crate) fn push(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        commits: u32,
    ) -> Result<PushDone, String> {
        // classify never makes a third-party reference's verdict a push (it
        // reads local-only); a second line, before anything is read, should
        // that slip (`a_third_party_push_fails_at_act_time_whatever_the_verdict`)
        if !self.entries[i].writable {
            return Err(format!(
                "{} is a third-party reference's, which is never pushed",
                b.name
            ));
        }
        let ready = match self.ready(i, facts, b)? {
            Ok(ready) => ready,
            Err(by) => return Ok(PushDone::Stopped(Stop::Held(by))),
        };
        let Some(target) = push_target(ready.branch) else {
            // classify leaves it to a person; never a guess at a ref
            return Err(format!("{}'s upstream isn't a branch on origin", b.name));
        };
        // a push rewrites no working tree: no lazy fetch
        let step = Step::new(self.git, self.root, &b.name, &facts.common_dir, None);
        step.push(
            Path::new(&facts.path),
            &Push {
                oid: &ready.branch.oid,
                upstream: ready.upstream,
                target,
                commits,
                shallow: facts.layout.shallow,
                url: &self.entries[i].url,
                batch_ssh: facts.config.batch_ssh(self.git.env_configures_ssh()),
            },
        )
    }

    /// Fast-forwards or moves branch `b` of entry `i` to its upstream's tip,
    /// in the one checkout it's on or in place, re-checking first.
    fn update(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        kind: UpdateKind,
    ) -> Result<UpdateDone, String> {
        let ready = match self.ready(i, facts, b)? {
            Ok(ready) => ready,
            Err(by) => return Ok(UpdateDone::Held(by)),
        };
        // a partial clone lacks the new tip's blobs its checkout needs:
        // fetched on demand from origin alone, over origin's transport
        let lazy = lazy_fetch(
            &facts.config,
            self.git.env_configures_ssh(),
            &self.entries[i].url,
        );
        let step = Step::new(self.git, self.root, &b.name, &facts.common_dir, lazy);
        let on = &ready.on;
        debug_assert!(
            on.len() <= 1,
            "{} acts on several checkouts: {on:?}",
            b.name
        );
        let checkout = match on[..] {
            [] => None,
            [c] => Some(c),
            // unreachable, but never a guess at which checkout
            _ => return Err(format!("{} is checked out in several checkouts", b.name)),
        };
        let upstream = ready.upstream;
        match (kind, checkout) {
            (UpdateKind::Move, None) => step.move_in_place(Path::new(&facts.path), upstream),
            (UpdateKind::Move, Some(c)) => step.move_in_checkout(Path::new(c), upstream),
            (UpdateKind::FastForward, None) => step.ff_in_place(Path::new(&facts.path), upstream),
            (UpdateKind::FastForward, Some(c)) => step.ff_in_checkout(Path::new(c), upstream),
        }
    }

    /// What a push and an update both read before git runs, in order: the
    /// branch as probed, its upstream, and the probed checkouts on it —
    /// then the live sessions re-read (`busy_now`), the inner `Err` holding
    /// the action. The outer `Err` fails it.
    fn ready<'f>(
        &self,
        i: usize,
        facts: &'f RepoFacts,
        b: &BranchStatus,
    ) -> Result<Result<Ready<'f>, SyncHold>, String> {
        let Some(branch) = facts.branches.iter().find(|f| f.branch.name == b.name) else {
            return Err(format!("{} isn't among the branches probed", b.name));
        };
        let Some(upstream) = branch.branch.upstream_ref.as_deref() else {
            return Err(format!("{} has no upstream to move to", b.name));
        };
        // the probed checkouts on it, from the facts classify read: it held
        // a fast-forward or move on several, or on an unprobed one; a push
        // on several goes on, since it moves no files
        let on = checkouts_on(facts, &b.name);
        if let Some(by) = self.busy_now(i, &b.name, &on) {
            return Ok(Err(by));
        }
        Ok(Ok(Ready {
            branch: &branch.branch,
            upstream,
            on,
        }))
    }

    /// Creates branch `b` of entry `i` on the registry's repo and makes it
    /// its upstream (`Step::create`), re-checking first as a push does —
    /// the one way `repos push --new-branch` creates a remote branch.
    /// `upstream`: the one it has now, none or origin's same-named branch,
    /// gone (`creatable`).
    pub(crate) fn create(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        upstream: NewBranchUpstream<'_>,
    ) -> PushOutcome {
        let failed = |message: String| PushOutcome::Failed { message };
        // `repos push` refuses a third-party target before anything runs; a
        // second line, as `push`'s
        if !self.entries[i].writable {
            return failed(format!(
                "{} is a third-party reference's, which is never pushed",
                b.name
            ));
        }
        let Some(branch) = facts.branches.iter().find(|f| f.branch.name == b.name) else {
            return failed(format!("{} isn't among the branches probed", b.name));
        };
        if let Some(by) = self.busy_now(i, &b.name, &checkouts_on(facts, &b.name)) {
            return PushOutcome::Held { by };
        }
        let step = Step::new(self.git, self.root, &b.name, &facts.common_dir, None);
        let created = step.create(
            Path::new(&facts.path),
            &NewBranch {
                oid: &branch.branch.oid,
                upstream,
                url: &self.entries[i].url,
                batch_ssh: facts.config.batch_ssh(self.git.env_configures_ssh()),
            },
        );
        match created {
            Ok(Creation::Created(to)) => PushOutcome::Created { to },
            Ok(Creation::Unmapped) => PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Unmapped,
            },
            Ok(Creation::Exists(at)) => PushOutcome::RemoteBranchExists { at },
            Ok(Creation::Stopped(Stop::Held(by))) => PushOutcome::Held { by },
            Ok(Creation::Stopped(Stop::PushFailed(failure))) => PushOutcome::PushFailed { failure },
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

/// Which update an `act` fast-forward or move makes (`Actor::update`).
#[derive(Debug, Clone, Copy)]
enum UpdateKind {
    FastForward,
    /// A shallow branch's move to the fetched tip.
    Move,
}

/// What an action read from the facts before git runs (`Actor::ready`).
struct Ready<'f> {
    /// The branch as probed.
    branch: &'f RefFacts,
    /// Its resolved upstream ref, a remote-tracking ref under
    /// `refs/remotes/origin/` (its name there may differ from the branch's).
    upstream: &'f str,
    /// The probed checkouts on it (`checkouts_on`).
    on: Vec<&'f str>,
}

/// The probed checkouts with `branch` on HEAD, from the facts classify
/// read.
fn checkouts_on<'f>(facts: &'f RepoFacts, branch: &str) -> Vec<&'f str> {
    let on_branch = |head: &Head| matches!(head, Head::Branch { name } if name == branch);
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
    on
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

mod step;
use step::{Creation, NewBranch, Push, Step, UpdateDone, lazy_fetch};
pub(crate) use step::{NewBranchUpstream, PushDone, Stop};

#[cfg(test)]
mod tests;
