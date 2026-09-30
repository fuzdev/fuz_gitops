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
//! **Never** a force-push, a tag pushed, a remote branch created, a rebase,
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
//! (`Actor::act`).
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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::busy::{Detection, EntryCheckouts, scope_sessions, sessions_under};
use crate::classify::{Refresh, lazy_transport, origin_matches, push_target, push_urls_match};
use crate::clone::{CLONE_TIMEOUT, Cloner};
use crate::discover::{Locate, Workspace, resolve_targets};
use crate::error;
use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::porcelain::{self, ConfigFacts};
use crate::probe::{RepoFacts, STATUS_ARGS, canonical, read_push_urls, read_shallow_roots};
use crate::registry::{Entry, RegistryDirs, RepoUrl};
use crate::remote::{RefspecContext, RemoteFailure};
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

/// The timeout for an action that rewrites a working tree (`merge
/// --ff-only`, `switch -C`), in place of `LOCAL_TIMEOUT`.
///
/// A checkout of a large tree through the user's filter drivers (Git LFS
/// fetching content) can take minutes, and git killed mid-checkout leaves
/// the files half-written against an unmoved HEAD. Long enough for any
/// checkout that is making progress; a hung filter still ends.
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
    /// one way sync and `repos push` act.
    pub(crate) fn act(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        action: SyncAction,
    ) -> BranchOutcome {
        let held = |by| BranchOutcome::Held { action, by };
        let failed = |message: String| BranchOutcome::Failed { action, message };
        // classify never makes a third-party reference's verdict a push (it
        // reads local-only); a second line, before anything is read, should
        // that slip (`a_third_party_push_fails_at_act_time_whatever_the_verdict`)
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
        let on = checkouts_on(facts, &b.name);
        if let Some(by) = self.busy_now(i, &b.name, &on) {
            return held(by);
        }
        // a partial clone lacks the new tip's blobs its checkout needs:
        // fetched on demand from origin alone, over origin's transport
        let lazy = lazy_fetch(
            &facts.config,
            self.git.env_configures_ssh(),
            &self.entries[i].url,
        );
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

    /// Creates branch `b` of entry `i` on the registry's repo and sets its
    /// upstream (`Step::create`), re-checking first as a push does — the
    /// one way `repos push --new-branch` creates a remote branch.
    /// `set_upstream`: the branch has no upstream configured (else its
    /// same-named upstream on origin is gone).
    pub(crate) fn create(
        &self,
        i: usize,
        facts: &RepoFacts,
        b: &BranchStatus,
        set_upstream: bool,
    ) -> PushOutcome {
        let failed = |message: String| PushOutcome::Failed { message };
        // `repos push` refuses a third-party target before anything runs; a
        // second line, as `act`'s
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
        let upstream = branch.branch.upstream_ref.as_deref().unwrap_or_default();
        let step = Step::new(
            self.git,
            self.root,
            &b.name,
            upstream,
            &facts.common_dir,
            None,
        );
        let created = step.create(
            Path::new(&facts.path),
            &NewBranch {
                oid: &branch.branch.oid,
                set_upstream,
                url: &self.entries[i].url,
                batch_ssh: !facts.config.ssh_command && !self.git.env_configures_ssh(),
            },
        );
        match created {
            Ok(Creation::Created(to)) => PushOutcome::Created { to },
            Ok(Creation::Unmapped) => PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Unmapped,
            },
            Ok(Creation::Exists(at)) => PushOutcome::RemoteBranchExists { at },
            Ok(Creation::Stopped(Done::Held(by))) => PushOutcome::Held { by },
            Ok(Creation::Stopped(Done::PushFailed(failure))) => PushOutcome::PushFailed { failure },
            // never a creation's: a bug, reported rather than passed over
            Ok(Creation::Stopped(done)) => failed(format!("the creation came out as {done:?}")),
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

/// How an action went, short of failing.
#[derive(Debug)]
enum Done {
    /// The branch moved from `from` to `to`.
    Updated { from: String, to: String },
    /// The remote's branch moved from `from`, the fetched tip, to `to`.
    Pushed { from: String, to: String },
    /// The push failed at the remote, or reaching it.
    PushFailed(RemoteFailure),
    /// The branch already held the tip.
    AlreadyThere,
    /// A re-check held it.
    Held(SyncHold),
}

/// The variables `Step::mapped_upstream`'s `--config-env` reads the
/// upstream it tries from.
const UPSTREAM_REMOTE_VAR: &str = "REPOS_UPSTREAM_REMOTE";
const UPSTREAM_MERGE_VAR: &str = "REPOS_UPSTREAM_MERGE";

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
    lazy: Option<LazyFetch<'a>>,
}

/// How an action that rewrites a partial clone's working tree fetches the
/// objects it lacks: from its promisor remote, origin — only when no other
/// remote is one (`ConfigFacts::other_promisor`), since git asks each in
/// turn — over `transport` alone (`lazy_transport`), and only while origin
/// still reaches `repo` over it (`Step::lazy_origin_moved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LazyFetch<'a> {
    transport: &'static str,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    batch_ssh: bool,
    /// The registry's repo: what origin must name when the fetch runs.
    repo: &'a RepoUrl,
}

/// A partial clone's lazy fetch, from its `config`: `None` — lazy fetching
/// stays off, and a checkout needing a missing blob fails — for a repo
/// that isn't a partial clone, one with another promisor remote, and one
/// whose origin, as git resolves it (`ConfigFacts::origin_fetch_url`,
/// rewrites applied: where the fetch connects), names no transport the
/// fetch may take (`lazy_transport`), or wasn't read. `env_ssh`: the
/// environment configures SSH; `repo`: the entry's.
///
/// Only when classify let the branch act, so with that URL naming the
/// registry's repo over SSH or HTTPS (`fetch_url_mismatch` holds an owned
/// entry, and a reference's refresh is held unless it's HTTPS) — as the
/// probe read it: right before the checkout, it's read again, and so is
/// whether another promisor remote is configured
/// (`Step::lazy_origin_moved`).
fn lazy_fetch<'a>(config: &ConfigFacts, env_ssh: bool, repo: &'a RepoUrl) -> Option<LazyFetch<'a>> {
    if config.partial_filter.is_none() || config.other_promisor {
        return None;
    }
    Some(LazyFetch {
        transport: lazy_transport(config.origin_fetch_url.as_deref()?)?,
        batch_ssh: !config.ssh_command && !env_ssh,
        repo,
    })
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
    /// The upstream's ref on origin (`push_target`), named explicitly, and
    /// the ref the lease is on.
    target: &'a str,
    /// The commits ahead the verdict counted.
    commits: u32,
    /// A shallow clone, where the verdict counted commits on no remote ref.
    shallow: bool,
    /// The registry's repo: where the push goes (its SSH URL), and what
    /// origin's push URL must name.
    url: &'a RepoUrl,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    batch_ssh: bool,
}

/// What a new branch's creation sends, as classified (`Step::create`).
struct NewBranch<'a> {
    /// The commit the branch held when probed: the one the remote branch
    /// is created at.
    oid: &'a str,
    /// The branch has no upstream configured, so creating it sets one;
    /// else its upstream is origin's same-named branch, gone.
    set_upstream: bool,
    /// The registry's repo: where the branch is created (its SSH URL), and
    /// what origin's push URL must name.
    url: &'a RepoUrl,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    batch_ssh: bool,
}

/// One `git send-pack` of a commit to a ref on the registry's repo.
struct SendPack<'a> {
    oid: &'a str,
    /// The ref on the registry's repo, named explicitly.
    target: &'a str,
    /// What the lease expects the remote's ref to be: the fetched tip, or
    /// `""` for no such ref.
    expect: &'a str,
    url: &'a RepoUrl,
    batch_ssh: bool,
}

/// What a send-pack came to, short of failing.
#[derive(Debug)]
enum Sent {
    /// The remote's ref moved to the commit.
    Pushed,
    /// The remote's ref already held it.
    UpToDate,
    /// Refused or not sent: held, or failed at the remote (`rejected`).
    Stopped(Done),
}

/// How creating a branch on the remote went, short of failing.
#[derive(Debug)]
enum Creation {
    /// The remote branch is at the commit, and its upstream set.
    Created(String),
    /// Origin's fetch refspec maps the branch to no remote-tracking ref,
    /// so it couldn't track what it would create.
    Unmapped,
    /// The fetch found origin holding a branch by that name, at another
    /// commit (the one held): never overwritten, or adopted.
    Exists(String),
    /// Held, or failed at the remote.
    Stopped(Done),
}

/// The push's command and flags before the lease, the URL, and the
/// refspec.
///
/// `git send-pack`, the plumbing under `git push`, because it connects to
/// the URL it's given as written. `git push <url>` reads a URL through the
/// remote config first: `url.<base>.insteadOf` and `pushInsteadOf` rewrite
/// it, and a `remote.<url>` section named by the URL itself takes it over
/// — from any config file, read when git runs, so a config written after
/// the push URL was checked could send the push elsewhere. Send-pack
/// consults neither, so the push reaches the registry's repo or fails.
///
/// And nothing but the one ref: send-pack pushes no tags
/// (`push.followTags` is `git push`'s), no submodules, and no push options
/// (it sends only the ones named with `--push-option`, never
/// `push.pushOption`), and runs no `pre-push` hook. `--no-signed`: no push
/// certificate (a signing prompt under a batch run, and a host that takes
/// none fails the push). `--receive-pack` names git's own remote command.
/// `--thin`, as `git push` packs. `--helper-status` prints one line per
/// remote ref on stdout (`pushed_ref`). SSH runs as the user configures it
/// (`core.sshCommand`, `~/.ssh/config`), their own program.
const SEND_PACK_ARGS: [&str; 5] = [
    "send-pack",
    "--helper-status",
    "--thin",
    "--receive-pack=git-receive-pack",
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
        lazy: Option<LazyFetch<'a>>,
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
        if self.lazy_origin_moved(checkout)? {
            return Ok(Done::Held(SyncHold::Changed));
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
        if self.has_local_work(checkout, &from)? || self.lazy_origin_moved(checkout)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        self.switch_reset(checkout, from, to)
    }

    /// Whether the lazy fetch a checkout would make may no longer reach
    /// the registry's repo over the transport decided, or alone: origin's
    /// URL, read again as git resolves it (`insteadOf` applied) — the URL
    /// the fetch connects to — names another repo, or another transport; or
    /// another promisor remote is configured now, which git would ask too.
    /// `false` with no lazy fetch: nothing reaches a remote.
    fn lazy_origin_moved(&self, dir: &Path) -> Result<bool, String> {
        let Some(lazy) = self.lazy else {
            return Ok(false);
        };
        let out = self
            .git
            .output_string(dir, &["ls-remote", "--get-url", "origin"], self.opts)
            .map_err(|e| git_message(&e))?;
        let url = out.trim_end_matches('\n');
        if !origin_matches(url, lazy.repo) || lazy_transport(url) != Some(lazy.transport) {
            return Ok(true);
        }
        let out = self
            .git
            .run(
                dir,
                &[
                    "config",
                    "-z",
                    "--show-scope",
                    "--show-origin",
                    "--get-regexp",
                    porcelain::CONFIG_PATTERN,
                ],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        // exit 1 is "no matching keys"
        if !out.status.success() && out.status.code() != Some(1) {
            return Err(first_message(&out.stderr));
        }
        let config = ConfigFacts::parse(&out.stdout, |_| false)?;
        Ok(config.other_promisor)
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

    /// Pushes `p.oid` to `p.target` on the registry's repo under a lease on
    /// the fetched tip, once the branch reads as classified — the same
    /// commit, upstream, and target, no symbolic ref — origin's push URL
    /// still names the registry's repo over SSH, and the commit is still the
    /// counted commits ahead of the remote-tracking ref, which is an
    /// ancestor of it. Then the remote-tracking ref moves to it
    /// (`record_push`). The module doc says what the lease and the ancestor
    /// check make of the push.
    fn push(&self, dir: &Path, p: &Push<'_>) -> Result<Done, String> {
        if !self.reads_as_classified(dir, p)? {
            return Ok(Done::Held(SyncHold::Changed));
        }
        // origin's push going elsewhere is a person's to sort out; the push
        // itself never reads it
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
        // the lease lifts git's fast-forward check: this is it
        if !self.is_ancestor(dir, &fetched, p.oid)? || ahead != p.commits as usize {
            return Ok(Done::Held(SyncHold::Changed));
        }
        let sent = self.send_pack(
            dir,
            &SendPack {
                oid: p.oid,
                target: p.target,
                expect: &fetched,
                url: p.url,
                batch_ssh: p.batch_ssh,
            },
        )?;
        // best effort, either way: the push stands whatever the ref says,
        // and the next fetch writes what origin holds
        match sent {
            Sent::Pushed => {
                let _ = self.record_push(dir, &fetched, p.oid);
                // the lease held the remote at the fetched tip
                Ok(Done::Pushed {
                    from: fetched,
                    to: p.oid.to_owned(),
                })
            }
            // the remote holds the commit: the lease would have refused
            // that, so another hand pushed it in the instant between — and
            // the fetched tip was an ancestor of it, so the ref moves as if
            // this push had put it there
            Sent::UpToDate => {
                let _ = self.record_push(dir, &fetched, p.oid);
                Ok(Done::AlreadyThere)
            }
            Sent::Stopped(done) => Ok(done),
        }
    }

    /// Creates the branch on the registry's repo — `refs/heads/<b>`, the
    /// same name — at `n.oid`, under a lease that it doesn't exist there,
    /// then sets its upstream as `git push -u` does: the remote-tracking ref
    /// its fetch refspec maps it to, created by compare-and-swap on none,
    /// then, when it had no upstream, `branch.<b>.remote` and `.merge`.
    ///
    /// First, as a push re-checks: the branch reads as classified — the
    /// same commit, a plain ref, and still no upstream configured (or its
    /// same-named upstream on origin, gone) — and origin's push URL still
    /// names the registry's repo over SSH. Then the remote-tracking ref it
    /// would track: none that origin's fetch refspec maps it to holds it
    /// for a person (`Unmapped`), and one there already, at another commit,
    /// is a branch the fetch found on origin, never adopted (`Exists`).
    ///
    /// The lease is a compare-and-swap on nothing: a branch created on the
    /// remote since the fetch fails it (`changed`), never overwritten, and
    /// one there at this very commit (another hand's, or this tool's own
    /// before its upstream was set) reads up to date, and the upstream is
    /// set all the same — so a run stopped between the push and the
    /// upstream is finished by the next.
    fn create(&self, dir: &Path, n: &NewBranch<'_>) -> Result<Creation, String> {
        let target = self.local();
        let upstream = if n.set_upstream {
            ["", "", ""]
        } else {
            [self.upstream, "origin", target.as_str()]
        };
        if !self.reads_as(dir, n.oid, upstream)? {
            return Ok(Creation::Stopped(Done::Held(SyncHold::Changed)));
        }
        if n.set_upstream && self.merge_configured(dir)? {
            return Ok(Creation::Stopped(Done::Held(SyncHold::Changed)));
        }
        if !push_urls_match(&read_push_urls(self.git, dir, self.opts)?, n.url) {
            return Ok(Creation::Stopped(Done::Held(SyncHold::PushUrl)));
        }
        let tracking = if n.set_upstream {
            match self.mapped_upstream(dir)? {
                Some(tracking) => tracking,
                None => return Ok(Creation::Unmapped),
            }
        } else {
            self.upstream.to_owned()
        };
        let tracked = self.resolve_ref(dir, &tracking)?;
        match &tracked {
            Some(at) if at != n.oid => return Ok(Creation::Exists(at.clone())),
            _ => {}
        }
        let sent = self.send_pack(
            dir,
            &SendPack {
                oid: n.oid,
                target: &target,
                expect: "",
                url: n.url,
                batch_ssh: n.batch_ssh,
            },
        )?;
        if let Sent::Stopped(done) = sent {
            return Ok(Creation::Stopped(done));
        }
        // as `git push -u`: the remote-tracking ref, then the upstream. The
        // ref is best effort, as a push's: a fetch that wrote it meanwhile
        // wrote what origin holds, and the next fetch writes it anyway
        // (until then, local refs read the upstream gone)
        if tracked.is_none() {
            let _ = self.update_tracking(dir, &tracking, n.oid, "");
        }
        if n.set_upstream {
            self.set_upstream(dir).map_err(|e| {
                format!(
                    "{target} is on origin at {}, but setting {}'s upstream failed: {e} — \
                     rerun repos push --new-branch to set it",
                    n.oid, self.branch
                )
            })?;
        }
        Ok(Creation::Created(n.oid.to_owned()))
    }

    /// Sends `s.oid` to `s.target` on the registry's repo, over SSH, under a
    /// lease that the remote's ref is `s.expect` (`""`: that it doesn't
    /// exist), and reads what git and the remote made of it.
    fn send_pack(&self, dir: &Path, s: &SendPack<'_>) -> Result<Sent, String> {
        let lease = format!("--force-with-lease={}:{}", s.target, s.expect);
        let url = s.url.ssh();
        let refspec = format!("{}:{}", s.oid, s.target);
        let mut args = SEND_PACK_ARGS.to_vec();
        args.extend([lease.as_str(), url.as_str(), refspec.as_str()]);
        let opts = CallOptions {
            network: Some(NetworkOptions {
                batch_ssh: s.batch_ssh,
            }),
            // the registry's URL is SSH: nothing else may carry the push
            allow_protocol: Some("ssh"),
            ..self.opts
        };
        let out = match self.git.run(dir, &args, opts) {
            Ok(out) => out,
            Err(e) => {
                return Ok(Sent::Stopped(Done::PushFailed(
                    RemoteFailure::from_git_error(e, RefspecContext::default()),
                )));
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        match pushed_ref(&stdout, s.target) {
            Some(PushedRef {
                ok: true,
                message: None,
            }) => Ok(Sent::Pushed),
            Some(PushedRef {
                ok: true,
                message: Some("up to date"),
            }) => Ok(Sent::UpToDate),
            Some(PushedRef {
                ok: true,
                message: Some(message),
            }) => Err(format!("git pushed {}: {message}", s.target)),
            Some(PushedRef { ok: false, message }) => Ok(Sent::Stopped(rejected(
                message.unwrap_or_default(),
                &out.stderr,
            ))),
            None if out.status.success() => {
                Err(format!("git send-pack reported nothing for {}", s.target))
            }
            None => Ok(Sent::Stopped(Done::PushFailed(
                RemoteFailure::from_git_error(
                    GitError::Failed {
                        args: args.join(" "),
                        code: out.status.code(),
                        stderr: out.stderr,
                    },
                    RefspecContext::default(),
                ),
            ))),
        }
    }

    /// The commit `r` holds in `dir`, or `None` when there's no such ref.
    fn resolve_ref(&self, dir: &Path, r: &str) -> Result<Option<String>, String> {
        let out = self
            .git
            .run(
                dir,
                &["rev-parse", "--verify", "--quiet", "--end-of-options", r],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
            // `--quiet`: 1, silently, for a ref that doesn't exist
            Some(1) => Ok(None),
            _ => Err(first_message(&out.stderr)),
        }
    }

    /// Whether `branch.<b>.merge` is set anywhere git reads config: the
    /// branch has an upstream (or half of one) someone configured.
    fn merge_configured(&self, dir: &Path) -> Result<bool, String> {
        let key = format!("branch.{}.merge", self.branch);
        let out = self
            .git
            .run(dir, &["config", "--get-all", &key], self.opts)
            .map_err(|e| git_message(&e))?;
        // 1 for a key that isn't set
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(first_message(&out.stderr)),
        }
    }

    /// The remote-tracking ref the branch would track as `origin`'s
    /// `refs/heads/<b>` — what git resolves as its upstream with that
    /// configured, through origin's fetch refspec — or `None` when the
    /// refspec maps it nowhere under `refs/remotes/origin/`.
    ///
    /// Read with the config passed for this call alone as `--config-env`,
    /// which git splits at the last `=` and whose values come whole from
    /// the environment: a branch named `a=b` reads as itself, where `-c`
    /// would split it at its first `=`.
    fn mapped_upstream(&self, dir: &Path) -> Result<Option<String>, String> {
        let name = self.branch;
        let local = self.local();
        let remote = format!("--config-env=branch.{name}.remote={UPSTREAM_REMOTE_VAR}");
        let merge = format!("--config-env=branch.{name}.merge={UPSTREAM_MERGE_VAR}");
        let env = [
            (UPSTREAM_REMOTE_VAR, "origin"),
            (UPSTREAM_MERGE_VAR, local.as_str()),
        ];
        let opts = CallOptions {
            env: &env,
            ..self.opts
        };
        let out = self
            .git
            .output_string(
                dir,
                &[
                    &remote,
                    &merge,
                    "for-each-ref",
                    "--format=%(refname)%00%(upstream)",
                    &local,
                ],
                opts,
            )
            .map_err(|e| git_message(&e))?;
        // the pattern also matches refs under it (`<b>/x`): only the ref
        let upstream = out
            .lines()
            .filter_map(|l| l.split_once('\0'))
            .find(|(r, _)| *r == local)
            .map(|(_, upstream)| upstream);
        Ok(upstream
            .filter(|u| u.starts_with("refs/remotes/origin/"))
            .map(str::to_owned))
    }

    /// Sets the branch's upstream to origin's same-named branch, as `git
    /// push -u` does: `branch.<b>.remote`, then `.merge` — in that order, so
    /// a run stopped between leaves the merge unset, which still reads as
    /// no upstream (and the next `--new-branch` finishes it).
    fn set_upstream(&self, dir: &Path) -> Result<(), String> {
        let local = self.local();
        for (key, value) in [("remote", "origin"), ("merge", local.as_str())] {
            let key = format!("branch.{}.{key}", self.branch);
            self.run_in(dir, &["config", "--replace-all", &key, value], self.opts)?;
        }
        Ok(())
    }

    /// Moves the remote-tracking ref to `pushed`, as `git push` records a
    /// push, by compare-and-swap on `fetched`: a fetch that moved it in the
    /// meantime wrote what origin holds, and stands. Only a ref under
    /// `refs/remotes/origin/`, which classify's push verdict implies.
    /// Returns whether it moved.
    fn record_push(&self, dir: &Path, fetched: &str, pushed: &str) -> Result<bool, String> {
        self.update_tracking(dir, self.upstream, pushed, fetched)
    }

    /// Moves `tracking`, a remote-tracking ref, to `new` by compare-and-swap
    /// on `old` (`""`: that it doesn't exist), as `git push` records a push.
    /// Only a ref under `refs/remotes/origin/`. Returns whether it moved.
    fn update_tracking(
        &self,
        dir: &Path,
        tracking: &str,
        new: &str,
        old: &str,
    ) -> Result<bool, String> {
        if !tracking.starts_with("refs/remotes/origin/") {
            return Ok(false);
        }
        let out = self
            .git
            .run(
                dir,
                &[
                    "update-ref",
                    "--no-deref",
                    "-m",
                    "repos: update by push",
                    tracking,
                    new,
                    old,
                ],
                self.opts,
            )
            .map_err(|e| git_message(&e))?;
        Ok(out.status.success())
    }

    /// Whether the branch in `dir` is as classified: the commit `p.oid`, a
    /// plain ref, its upstream the same remote-tracking ref, on `origin`,
    /// at `p.target` there. `false` when deleted since.
    fn reads_as_classified(&self, dir: &Path, p: &Push<'_>) -> Result<bool, String> {
        self.reads_as(dir, p.oid, [self.upstream, "origin", p.target])
    }

    /// Whether the branch in `dir` holds `oid`, is a plain ref, and has
    /// `upstream` — its resolved remote-tracking ref, the remote, and the
    /// ref there, each `""` for none. `false` when deleted since.
    fn reads_as(&self, dir: &Path, oid: &str, upstream: [&str; 3]) -> Result<bool, String> {
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
        let [tracking, remote, remote_ref] = upstream;
        let expected = [local.as_str(), oid, "", tracking, remote, remote_ref];
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

/// One ref's line in `git send-pack --helper-status`'s output: `ok <ref>`,
/// `ok <ref> up to date`, or `error <ref> <why>` — git's reason (`stale
/// info`; `no match` for each remote ref not pushed) or the remote's own.
#[derive(Debug, PartialEq, Eq)]
struct PushedRef<'a> {
    ok: bool,
    /// What follows the ref, its surrounding quotes dropped (git C-quotes a
    /// message holding special characters).
    message: Option<&'a str>,
}

/// The status line for the push to `dst`, if git printed one.
fn pushed_ref<'a>(stdout: &'a str, dst: &str) -> Option<PushedRef<'a>> {
    stdout.lines().find_map(|l| {
        let (status, rest) = l.split_once(' ')?;
        let ok = match status {
            "ok" => true,
            "error" => false,
            _ => return None,
        };
        let (r, message) = rest
            .split_once(' ')
            .map_or((rest, None), |(r, m)| (r, Some(m)));
        let unquoted = |m: &'a str| {
            m.strip_prefix('"')
                .and_then(|m| m.strip_suffix('"'))
                .unwrap_or(m)
        };
        (r == dst).then(|| PushedRef {
            ok,
            message: message.map(unquoted),
        })
    })
}

/// A push git or the remote refused, by why: the remote's branch isn't the
/// fetched tip (`stale info`, the lease's refusal — moved or deleted since
/// the fetch; `fetch first` and `non-fast forward`, git's own, should a
/// lease ever not apply) is held, `Changed`, for a rerun to reclassify;
/// git's other refusals fail with their words; anything else is the
/// remote's refusal (a ruleset, a hook), failed with its reason and the
/// remote's first error line. A remote whose reason reads exactly as one
/// of git's is taken for git's: nothing was pushed either way.
fn rejected(why: &str, stderr: &str) -> Done {
    match why {
        "stale info" | "fetch first" | "non-fast forward" => Done::Held(SyncHold::Changed),
        "needs force"
        | "already exists"
        | "remote ref updated since checkout"
        | "no match"
        | "expecting report"
        | "atomic push failed"
        | "" => Done::PushFailed(RemoteFailure::Failed {
            message: format!(
                "rejected: {}",
                if why.is_empty() { "no reason" } else { why }
            ),
        }),
        reason => Done::PushFailed(RemoteFailure::Rejected {
            reason: reason.to_owned(),
            message: remote_error(stderr),
        }),
    }
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
mod tests;
