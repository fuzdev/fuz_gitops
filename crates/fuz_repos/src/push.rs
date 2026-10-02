//! `repos push`: the gateway, the path an agent pushes by instead of `git
//! push`.
//!
//! It pushes the branch checked out in each target checkout, through
//! `sync`'s own push and under its policy, and nothing else.
//!
//! **The pipeline.** For the targets' entries alone, what `sync` runs before
//! it acts: probe each, fetching it first as `status --fetch` does (the
//! same hardened fetch, remote-tracking refs alone, and the visibility
//! check); read the live sessions after the fetches, the caller's own
//! excluded; classify. Then, for each target, read the branch its checkout
//! has checked out and act on that branch's push verdict alone, through
//! `Actor::push` — the one push `sync` makes, with every re-check it makes
//! right before (the live sessions, the branch as classified, origin's push
//! URL, the commits ahead) and its race closures (the lease on the fetched
//! tip, the registry's URL pushed to directly, the remote-tracking ref
//! moved by compare-and-swap: the `sync` module doc says how).
//!
//! **Never** a fast-forward, a move, a clone, or any branch but the one
//! checked out at a target: a branch behind is `NotAhead` (sync's to
//! fast-forward), a diverged one `NeedsHuman` (sync's to rebase when it's
//! the registry's branch, else a person's). The policy is sync's, and
//! structural: owned entries only (a third-party reference or a pin named
//! is refused before anything runs, `check_pushable`), never a force or a
//! tag, and a remote branch created only under `--new-branch`, below; a
//! checkout another live session works in holds the push (`busy`), and so
//! does origin drift (`entry`, or `push_url`). A branch's relation the run
//! can't vouch for — its entry held whole (origin drift among the
//! reasons), or its fetch failed — holds it whatever it reads, in sync
//! included (`entry`, `fetch_failed`), so the push never exits `0` on refs
//! that aren't origin's. Dirt doesn't matter: a push moves refs alone.
//!
//! **`--new-branch`, the user's**: a branch with no upstream on origin —
//! none configured (`NoUpstream`), or origin's same-named branch as its
//! upstream, deleted there (`gone`) with commits on no remote (with none,
//! it was merged, and stays `NoUpstream`) — is created on the registry's repo
//! under its own name and made its upstream, as `git push -u` does
//! (`Actor::create`, the `sync` module doc says how). Only such a branch:
//! one with a live upstream on origin pushes as it would without the flag,
//! and one tracking another remote, or origin's branch under another name,
//! stays `NoUpstream`. A branch origin already has at another commit is
//! never overwritten or adopted (`RemoteBranchExists`), and one the fetch
//! refspec leaves out can't be tracked (`NeedsHuman`, `unmapped`). Every
//! hold on a push holds a creation, the failed fetch included. In an
//! archived repo such a branch is `NeedsHuman` (`archived_ahead`), with or
//! without the flag. An agent is refused it before anything runs
//! (`check_new_branch`).
//!
//! **An agent may run it** without `--new-branch`: its push is classified
//! and made as a person's. It's the path agents push by: the user's Claude
//! Code settings deny them raw `git push`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::classify::{NeedsHuman, Refresh};
use crate::discover::{Locate, PathTarget, Workspace, resolve_push_targets};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::probe::RepoFacts;
use crate::registry::{Entry, RegistryDirs};
use crate::report::{
    BranchSyncHold, CheckoutPush, EntryStatus, FetchOutcome, NoUpstreamWhy, PushOutcome,
    PushReport, Sessions,
};
use crate::sessions::{Caller, LiveSessions, SessionsSource, read_live_sessions};
use crate::state::{BranchNeedsHuman, BranchStatus, Head, Relation, SyncAction, Verdict};
use crate::status::{EntryTiming, Reported, RunTimings, Survey, assemble_report, probe_and_assess};
use crate::sync::{Actor, NewBranchUpstream, PushDone, Stop, fetch_outcome};

/// How to run `push`.
#[derive(Clone, Copy)]
pub struct PushOptions<'a> {
    /// Entries probed and fetched at once, at least one.
    pub jobs: usize,
    /// As `StatusOptions::visibility_base`: a seam for tests.
    pub visibility_base: Option<&'a str>,
    /// Reads the live sessions (`read_live_sessions`): once the fetches are
    /// done, to classify, and again right before each push. A seam for
    /// tests.
    pub read_live: &'a (dyn Fn() -> LiveSessions + Sync),
    /// `--new-branch`: create a target's branch on origin when it has no
    /// upstream there to push to (`new_branch`) — the user's alone:
    /// `push_report` refuses it to an agent; `push` itself doesn't ask who
    /// runs it.
    pub new_branch: bool,
}

impl std::fmt::Debug for PushOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushOptions")
            .field("jobs", &self.jobs)
            .field("visibility_base", &self.visibility_base)
            .field("new_branch", &self.new_branch)
            .finish_non_exhaustive()
    }
}

/// What `push` found and did.
#[derive(Debug)]
pub struct PushRun {
    /// The targets' entries after the fetch, each once, in the order the
    /// targets first name them.
    pub entries: Vec<EntryStatus>,
    /// Busy detection as classified, after the fetch.
    pub sessions: Sessions,
    /// What the push did, one per target, in their order.
    pub pushes: Vec<CheckoutPush>,
    pub timings: Vec<EntryTiming>,
    /// Wall time of the probe pool (fetches included).
    pub probe_elapsed: Duration,
    /// Wall time of the acting.
    pub act_elapsed: Duration,
}

/// How to make `repos push`'s report (`push_report`).
#[derive(Debug, Clone, Copy)]
pub struct PushReportOptions<'a> {
    /// `--new-branch` (`PushOptions::new_branch`), refused to an agent.
    pub new_branch: bool,
    /// Who runs it (`Caller::from_env`): `new_branch` is the user's alone.
    pub caller: Caller,
    /// Entries fetched at once, at least one.
    pub jobs: usize,
    /// Where the live sessions are read (`SessionsSource::from_env`), after
    /// the fetches and again right before each push.
    pub sessions: &'a SessionsSource,
}

/// `repos push`'s report: the branch checked out at each checkout `targets`
/// name, pushed.
///
/// `new_branch` is refused to an agent before anything else
/// (`check_new_branch`). Then it loads the workspace (`Workspace::load`,
/// from `cwd`), resolves `targets` to checkouts (`resolve_push_targets`:
/// none is the checkout holding `cwd`) and refuses any not pushable
/// (`check_pushable`) before anything is fetched, pushes each checkout's
/// branch (`push`), and assembles the report, with no unregistered scan.
///
/// # Errors
///
/// `NewBranchByAgent`; what `Workspace::load` and `resolve_push_targets`
/// return; `PushThirdParty` or `PushPinned`. Nothing has been fetched when
/// it fails; a push that fails is the report's to say.
pub fn push_report(
    git: &Git,
    cwd: &Path,
    locate: Locate<'_>,
    targets: &[String],
    opts: PushReportOptions<'_>,
) -> Result<Reported<PushReport>> {
    let start = Instant::now();
    if opts.new_branch {
        check_new_branch(opts.caller)?;
    }
    let ws = Workspace::load(git, cwd, cwd, locate)?;
    let targets = resolve_push_targets(&ws.entries, ws.root(), cwd, targets, git)?;
    // a usage error, not a report
    check_pushable(&targets)?;
    let load = start.elapsed();

    let read_live = || read_live_sessions(opts.sessions);
    let run = push(
        &targets,
        &ws.registry_dirs(),
        ws.root(),
        git,
        PushOptions {
            jobs: opts.jobs,
            visibility_base: None,
            read_live: &read_live,
            new_branch: opts.new_branch,
        },
    );
    let status = assemble_report(&ws, true, run.sessions, run.entries, None);
    Ok(Reported {
        report: PushReport::new(status, run.pushes),
        timings: RunTimings {
            load,
            scan: None,
            probe: run.probe_elapsed,
            act: Some(run.act_elapsed),
            entries: run.timings,
        },
    })
}

/// Refuses targets `repos push` never pushes: a third-party reference's
/// checkout, or a pin's.
///
/// # Errors
///
/// `PushThirdParty` or `PushPinned`, for the first such target.
pub fn check_pushable(targets: &[PathTarget]) -> Result<()> {
    for t in targets {
        if !t.entry.writable {
            return Err(Error::PushThirdParty {
                key: t.entry.key.clone(),
            });
        }
        if t.entry.pinned {
            return Err(Error::PushPinned {
                key: t.entry.key.clone(),
            });
        }
    }
    Ok(())
}

/// Refuses `--new-branch` to an agent: creating a remote branch is the
/// user's, run by them (`Caller::from_env`).
///
/// # Errors
///
/// `NewBranchByAgent` when `caller` is an agent.
const fn check_new_branch(caller: Caller) -> Result<()> {
    match caller {
        Caller::Person => Ok(()),
        Caller::Agent => Err(Error::NewBranchByAgent),
    }
}

/// Fetches the targets' entries, classifies them, and pushes the branch
/// checked out at each target where its verdict is a push.
///
/// `targets` are resolved (`resolve_push_targets`) and pushable
/// (`check_pushable`); `registry_dirs` are the whole registry's dirs.
pub fn push(
    targets: &[PathTarget],
    registry_dirs: &RegistryDirs,
    root: &Path,
    git: &Git,
    opts: PushOptions<'_>,
) -> PushRun {
    // each entry once, in the order the targets first name it
    let mut entries: Vec<Entry> = Vec::new();
    let at: Vec<usize> = targets
        .iter()
        .map(|t| {
            entries
                .iter()
                .position(|e| e.key == t.entry.key)
                .unwrap_or_else(|| {
                    entries.push(t.entry.clone());
                    entries.len() - 1
                })
        })
        .collect();

    let (assessed, probe_elapsed) = probe_and_assess(
        &entries,
        Survey {
            git,
            root,
            registry_dirs,
            fetch: true,
            // owned entries, never pinned: nothing to refresh
            refresh: Refresh::Unasked,
            jobs: opts.jobs,
            visibility_base: opts.visibility_base,
            unregistered: &[],
        },
        opts.read_live,
    );

    let start = Instant::now();
    let actor = Actor {
        git,
        root,
        entries: &entries,
        checkouts: &assessed.checkouts,
        read_live: opts.read_live,
    };
    // by repo and branch: a branch pushes once, however many targets name it
    let mut done: HashMap<(PathBuf, String), PushOutcome> = HashMap::new();
    let pushes = targets
        .iter()
        .zip(at)
        .map(|(t, i)| {
            let fetch = fetch_outcome(assessed.fetches[i].as_ref());
            let (branch, outcome) = target_outcome(
                &actor,
                &Target {
                    i,
                    facts: assessed.facts[i].as_ref(),
                    status: &assessed.entries[i],
                    fetch: &fetch,
                    checkout: &t.checkout,
                    new_branch: opts.new_branch,
                },
                &mut done,
            );
            CheckoutPush {
                key: t.entry.key.clone(),
                checkout: t.checkout.to_string_lossy().into_owned(),
                branch,
                fetch,
                outcome,
            }
        })
        .collect();
    PushRun {
        entries: assessed.entries,
        sessions: assessed.sessions,
        pushes,
        timings: assessed.timings,
        probe_elapsed,
        act_elapsed: start.elapsed(),
    }
}

/// One target, as `target_outcome` reads it: entry `i`'s facts and status
/// after the fetch, how its `fetch` went, the `checkout` named, and whether
/// the run creates a branch with no upstream on origin (`new_branch`).
struct Target<'a> {
    i: usize,
    facts: Option<&'a RepoFacts>,
    status: &'a EntryStatus,
    fetch: &'a FetchOutcome,
    checkout: &'a Path,
    new_branch: bool,
}

/// The branch checked out at the target's checkout, and what pushing it
/// came to: pushed through `actor` when its verdict is a push — or, under
/// `--new-branch`, created on origin when it has no upstream there
/// (`creatable`) — else what the verdict says of it; unless the entry is
/// held whole or its fetch didn't land, which holds it whatever the
/// verdict. `done` holds the pushes already made, by repo
/// (`RepoFacts::repo_key`) and branch.
fn target_outcome(
    actor: &Actor<'_>,
    t: &Target<'_>,
    done: &mut HashMap<(PathBuf, String), PushOutcome>,
) -> (Option<String>, PushOutcome) {
    // missing, not a repo, or a probe that failed: no verdicts to act on
    let Some(facts) = t.facts else {
        return (None, PushOutcome::Unread);
    };
    // a worktree the probe couldn't read has no head to go by
    let Some(c) = t.status.checkout_at(t.checkout) else {
        return (None, PushOutcome::Unread);
    };
    let name = match &c.head {
        Head::Branch { name } => name,
        Head::Detached { .. } => return (None, PushOutcome::Detached),
    };
    let branch = Some(name.clone());
    let Some(b) = t.status.branches.iter().find(|b| b.name == *name) else {
        // a branch with no commit yet has no ref to push
        return (
            branch,
            PushOutcome::Failed {
                message: format!("{name} has no commit to push"),
            },
        );
    };
    // an alias never acts (`BranchStatus::symref`): its target is the
    // branch. A second line: status reads HEAD through every symbolic ref
    // (and `git switch` to an alias checks out its target), so a checkout
    // on an alias reads as on its target
    if let Some(target) = &b.symref {
        let message = format!("{name} is a symbolic ref to {target}: push that branch");
        return (branch, PushOutcome::Failed { message });
    }
    // classify holds only an action, so a branch with none pending reads
    // quiet from whatever refs the run has: when those aren't origin's —
    // an entry-level reason (origin drift among them) or a fetch that
    // didn't land — the branch is held as sync holds a push, in sync's
    // order (entry, push URL, fetch), never reported in sync. A push
    // verdict already held names its hold as sync would
    if t.status.needs_human.iter().any(NeedsHuman::holds_entry) {
        return (
            branch,
            PushOutcome::Held {
                by: BranchSyncHold::Entry,
            },
        );
    }
    if let Verdict::Held {
        action: SyncAction::Push { .. },
        by,
    } = &b.verdict
    {
        return (branch, PushOutcome::Held { by: (*by).into() });
    }
    let create = if t.new_branch {
        creatable(facts, t.status, b)
    } else {
        None
    };
    // no origin upstream configured is the branch's config, not the
    // fetch's: said whether or not the fetch landed (a gone upstream is
    // the fetch's to say). Unless the run creates it, which pushes
    if b.relation == Relation::Untracked && create.is_none() {
        return (branch, no_upstream(facts, t.status, b));
    }
    if *t.fetch != FetchOutcome::Fetched {
        let by = BranchSyncHold::FetchFailed;
        return (branch, PushOutcome::Held { by });
    }
    if let Some(upstream) = create {
        let outcome = if t.status.archived {
            // as a push to an archived repo: a person's
            PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead,
            }
        } else {
            done.entry((facts.repo_key.clone(), name.clone()))
                .or_insert_with(|| actor.create(t.i, facts, b, upstream))
                .clone()
        };
        return (branch, outcome);
    }
    let outcome = match &b.verdict {
        Verdict::Act {
            action: SyncAction::Push { commits },
        } => done
            .entry((facts.repo_key.clone(), name.clone()))
            .or_insert_with(|| pushed(actor.push(t.i, facts, b, *commits)))
            .clone(),
        Verdict::Held {
            action: SyncAction::Push { .. },
            by,
        } => PushOutcome::Held { by: (*by).into() },
        // diverged, sync's to rebase, held or not: never the push's
        Verdict::Act {
            action: SyncAction::Rebase { .. },
        }
        | Verdict::Held {
            action: SyncAction::Rebase { .. },
            ..
        } => PushOutcome::NeedsHuman {
            reason: BranchNeedsHuman::Diverged,
        },
        // behind, or a stale shallow pointer: sync's to move
        Verdict::Act { .. } | Verdict::Held { .. } => PushOutcome::NotAhead,
        Verdict::NeedsHuman { reason } => PushOutcome::NeedsHuman { reason: *reason },
        Verdict::Quiet if b.relation == Relation::InSync => PushOutcome::InSync,
        // commits on no remote, merged, its upstream gone, tracking another
        // remote, or none with nothing committed: no upstream on origin a
        // push may name
        Verdict::LocalOnly | Verdict::Cleanup { .. } | Verdict::Quiet => {
            no_upstream(facts, t.status, b)
        }
    };
    (branch, outcome)
}

/// Whether `--new-branch` creates branch `b` on origin, by the upstream it
/// has: `Unset` for a branch with no upstream configured
/// (`branch.<b>.merge` unset, whatever `.remote` says — `git push -u` sets
/// both), `Gone` for one whose upstream is origin's branch of the same
/// name, gone (deleted there, so the fetch pruned it), with commits on no
/// remote; `None` for anything else — a live upstream
/// pushes as usual, one on another remote, or origin's under another name,
/// is a person's to push, and a gone one with nothing on no remote was
/// merged and deleted there (GitHub deletes a merged PR's branch), so it
/// reads as it would without the flag: recreating it is by hand. A
/// squash-merged branch keeps its commits unique, so it's recreated —
/// unless it's the branch the entry follows (`default_branch_gone`): its
/// upstream gone is the remote's default renamed or deleted, a person's to
/// repoint, never put back.
fn creatable<'f>(
    facts: &'f RepoFacts,
    status: &EntryStatus,
    b: &BranchStatus,
) -> Option<NewBranchUpstream<'f>> {
    let config = facts.config.branches.get(&b.name);
    match b.relation {
        Relation::Untracked => config
            .is_none_or(|c| c.merge.is_none())
            .then_some(NewBranchUpstream::Unset),
        Relation::Gone if status.default_branch_gone(&b.name) => None,
        Relation::Gone => {
            let refs = facts.branches.iter().find(|f| f.branch.name == b.name)?;
            let tracking = refs.branch.upstream_ref.as_deref()?;
            let same = tracking == format!("refs/remotes/origin/{}", b.name)
                && refs.branch.merge_ref.as_deref()
                    == Some(format!("refs/heads/{}", b.name).as_str());
            // merged, and deleted on origin: nothing of it to put back
            (same && b.unique_commits > 0).then_some(NewBranchUpstream::Gone { tracking })
        }
        _ => None,
    }
}

/// A branch with no upstream on origin a push may name, not pushed, and why
/// (`NoUpstreamWhy`), read as `creatable` reads it: one `--new-branch`
/// would create (only on a run without the flag, which creates it), the
/// entry's own branch gone from origin, one gone with nothing on no remote
/// (merged), or one tracking elsewhere. One the flag would create in an
/// archived repo is `needs_human` (`archived_ahead`), as the flag reads it.
fn no_upstream(facts: &RepoFacts, status: &EntryStatus, b: &BranchStatus) -> PushOutcome {
    if creatable(facts, status, b).is_some() {
        return if status.archived {
            // as a push to an archived repo: a person's
            PushOutcome::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead,
            }
        } else {
            PushOutcome::NoUpstream {
                why: NoUpstreamWhy::Creatable,
            }
        };
    }
    let why = match b.relation {
        Relation::Gone if status.default_branch_gone(&b.name) => NoUpstreamWhy::DefaultGone,
        Relation::Gone if b.unique_commits == 0 => NoUpstreamWhy::Merged,
        _ => NoUpstreamWhy::OtherUpstream,
    };
    PushOutcome::NoUpstream { why }
}

/// A push's outcome (`Actor::push`) as `repos push` reports it.
fn pushed(result: std::result::Result<PushDone, String>) -> PushOutcome {
    match result {
        Ok(PushDone::Pushed { from, to }) => PushOutcome::Pushed { from, to },
        // already where the push would have put it
        Ok(PushDone::AlreadyThere) => PushOutcome::InSync,
        Ok(PushDone::Stopped(Stop::Held(by))) => PushOutcome::Held { by },
        Ok(PushDone::Stopped(Stop::PushFailed(failure))) => PushOutcome::PushFailed { failure },
        Err(message) => PushOutcome::Failed { message },
    }
}
