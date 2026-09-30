//! `repos status`: probe entries over a bounded thread pool, scope the live
//! sessions to their checkouts, and classify — the two phases `sync` runs
//! before it acts (`probe_all`, `assess`).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::busy::{
    EntryCheckouts, EntrySessions, Sessions, same_path, scope_sessions, sessions_under,
};
use crate::classify::{
    ClassifiedMissing, NeedsHuman, Refresh, classify, classify_missing, refresh_intent,
    refresh_verdict,
};
use crate::git::Git;
use crate::probe::{ProbeContext, ProbeRun, Probed, RegistryDirs, RepoFacts, RepoFetches, probe};
use crate::registry::Entry;
use crate::remote::{
    RemoteFailure, VisibilityCheck, is_declared_private, read_anonymously, visibility_url,
};
use crate::report::{EntryStatus, UnregisteredClone};
use crate::scan::Scan;
use crate::sessions::{Caller, LiveSessions};
use crate::state::{Checkout, Presence, Prune, RefreshVerdict};

/// How to run `status`.
#[derive(Debug, Clone, Copy)]
pub struct StatusOptions<'a> {
    /// Fetch from `origin` before probing — owned entries not pinned, and
    /// the third-party references `refresh` refreshes — and run the
    /// visibility check on each `[repos]` entry declared private.
    pub fetch: bool,
    /// Which references the run refreshes (`Refresh`): previewed as sync
    /// would refresh them, and fetched under `fetch`.
    pub refresh: Refresh,
    /// The unregistered scan's dirs, when it ran (without targets, or with
    /// a missing entry among them): a missing entry whose repo one of them
    /// clones is held.
    pub unregistered: Option<&'a [UnregisteredClone]>,
    /// Git calls in flight at once — entries probed, visibility checks —
    /// at least one.
    pub jobs: usize,
    /// The base the visibility check reads each repo under, as
    /// `<base><account>/<name>`, in place of its registry URL: a seam for
    /// tests, which point it at a `file://` dir or a local server. `None`
    /// reads the registry URL.
    pub visibility_base: Option<&'a str>,
    /// The live sessions the run scopes to checkouts
    /// (`read_live_sessions`), read by the caller: a seam for tests.
    pub live: &'a LiveSessions,
    /// Who runs the tool (`Caller::from_env`): an agent's pushes are
    /// previewed held for the gateway, as its sync would hold them.
    pub caller: Caller,
}

/// One entry's time, for `--timings`.
#[derive(Debug, Clone)]
pub struct EntryTiming {
    pub key: String,
    pub fetch: Duration,
    pub probe: Duration,
    /// The visibility check's; zero when it didn't run.
    pub visibility: Duration,
}

/// The entries' statuses, in the order given, with their timings.
#[derive(Debug)]
pub struct StatusRun {
    pub entries: Vec<EntryStatus>,
    /// Busy detection over the checkouts probed.
    pub sessions: Sessions,
    pub timings: Vec<EntryTiming>,
    /// Wall time of the whole pool.
    pub elapsed: Duration,
}

/// A pool's unit of work.
#[derive(Debug)]
enum Done {
    Entry(Box<ProbeRun>, EntryTiming),
    Visibility(usize, VisibilityCheck, Duration),
}

/// Probes `entries` over a pool of `opts.jobs` threads, then scopes
/// `opts.live` to the checkouts found and classifies each entry;
/// `registry_dirs` are the whole registry's dirs, whatever `entries` holds.
///
/// Under `opts.fetch` the visibility checks share the pool, queued ahead of
/// the entries so they overlap the fetches rather than trail them; each
/// runs in `root` (no repo's config applies to it).
pub fn status(
    entries: &[Entry],
    registry_dirs: &RegistryDirs,
    root: &Path,
    git: &Git,
    opts: StatusOptions<'_>,
) -> StatusRun {
    let start = Instant::now();
    let fetches = RepoFetches::default();
    let probes = probe_all(
        entries,
        ProbeContext {
            git,
            root,
            registry_dirs,
            fetch: opts.fetch,
            refresh: opts.refresh,
            fetches: &fetches,
        },
        opts.jobs,
        opts.visibility_base,
    );
    let assessed = assess(
        entries,
        probes,
        &Assess {
            root,
            live: opts.live,
            caller: opts.caller,
            refresh: opts.refresh,
            unregistered: opts.unregistered.unwrap_or_default(),
        },
    );
    StatusRun {
        entries: assessed.entries,
        sessions: assessed.sessions,
        timings: assessed.timings,
        elapsed: start.elapsed(),
    }
}

/// Every entry's probe, and under a fetching `cx` the visibility checks of
/// those declared private, in the order given.
#[derive(Debug)]
pub(crate) struct Probes {
    runs: Vec<(ProbeRun, EntryTiming)>,
    /// By entry index.
    checks: Vec<(usize, VisibilityCheck, Duration)>,
}

/// Runs `f` on each of `tasks` indices over a pool of `jobs` threads (at
/// least one, at most one per task), returning the results by index.
pub(crate) fn run_pool<T: Send>(
    tasks: usize,
    jobs: usize,
    f: impl Fn(usize) -> T + Sync,
) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let jobs = jobs.clamp(1, tasks.max(1));
    let mut done: Vec<(usize, T)> = thread::scope(|s| {
        let workers: Vec<_> = (0..jobs)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let task = next.fetch_add(1, Ordering::Relaxed);
                        if task >= tasks {
                            break;
                        }
                        out.push((task, f(task)));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            // a worker only panics on a bug; surface it rather than drop tasks
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, t)| t).collect()
}

/// Probes `entries` over a pool of `jobs` threads — fetching first when
/// `cx.fetch` says so, with the visibility checks queued ahead of the
/// entries, reading under `visibility_base` (`StatusOptions`).
pub(crate) fn probe_all(
    entries: &[Entry],
    cx: ProbeContext<'_>,
    jobs: usize,
    visibility_base: Option<&str>,
) -> Probes {
    // the entries the visibility check reads, by index
    let checks: Vec<usize> = if cx.fetch {
        (0..entries.len())
            .filter(|&i| is_declared_private(&entries[i]))
            .collect()
    } else {
        Vec::new()
    };
    let done = run_pool(checks.len() + entries.len(), jobs, |task| {
        let start = Instant::now();
        if let Some(&i) = checks.get(task) {
            let url = visibility_url(&entries[i], visibility_base);
            let check = read_anonymously(cx.git, cx.root, &url);
            return Done::Visibility(i, check, start.elapsed());
        }
        let entry = &entries[task - checks.len()];
        let run = probe(entry, cx);
        let timing = EntryTiming {
            key: entry.key.clone(),
            fetch: run.fetch_time,
            probe: run.probe_time,
            visibility: Duration::ZERO,
        };
        Done::Entry(Box::new(run), timing)
    });
    let mut probes = Probes {
        runs: Vec::with_capacity(entries.len()),
        checks: Vec::with_capacity(checks.len()),
    };
    // by task index: the checks, then the entries in order
    for d in done {
        match d {
            Done::Entry(run, timing) => probes.runs.push((*run, timing)),
            Done::Visibility(i, check, time) => probes.checks.push((i, check, time)),
        }
    }
    probes
}

/// What `assess` makes of the probes.
#[derive(Debug)]
pub(crate) struct Assessed {
    pub sessions: Sessions,
    pub entries: Vec<EntryStatus>,
    pub timings: Vec<EntryTiming>,
    /// Each entry's facts, when its repo was probed whole.
    pub facts: Vec<Option<RepoFacts>>,
    /// Each entry's fetch: `None` when none was attempted.
    pub fetches: Vec<Option<Result<(), RemoteFailure>>>,
    /// Each entry's checkouts, which busy detection scopes sessions to.
    pub checkouts: Vec<EntryCheckouts>,
}

/// What `assess` classifies against, beside the probes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assess<'a> {
    /// The workspace root, which missing entries' paths are under.
    pub root: &'a Path,
    pub live: &'a LiveSessions,
    pub caller: Caller,
    pub refresh: Refresh,
    /// The unregistered scan's dirs; empty when it didn't run.
    pub unregistered: &'a [UnregisteredClone],
}

/// Scopes `cx.live` to the probed checkouts, so a session lands in the
/// deepest of them all, and classifies each entry for `cx.caller` and
/// `cx.refresh` — a missing one's clone against the sessions at its path
/// under the root, the gone worktrees the probed entries record, and the
/// unregistered dirs (`classify_missing_at`).
pub(crate) fn assess(entries: &[Entry], probes: Probes, cx: &Assess<'_>) -> Assessed {
    let checkouts: Vec<EntryCheckouts> = probes
        .runs
        .iter()
        .map(|(run, _)| entry_checkouts(&run.probed))
        .collect();
    let (sessions, per_entry) = scope_sessions(cx.live, &checkouts);
    let mut assessed = Assessed {
        sessions,
        entries: Vec::with_capacity(entries.len()),
        timings: Vec::with_capacity(entries.len()),
        facts: Vec::with_capacity(entries.len()),
        fetches: Vec::with_capacity(entries.len()),
        checkouts,
    };
    for ((entry, (run, timing)), busy) in entries.iter().zip(probes.runs).zip(&per_entry) {
        assessed.facts.push(match &run.probed {
            Probed::Present(facts) => Some((**facts).clone()),
            _ => None,
        });
        assessed.fetches.push(run.fetch.clone());
        assessed
            .entries
            .push(entry_status(entry, run, busy, cx.caller, cx.refresh));
        assessed.timings.push(timing);
    }
    for (i, check, time) in probes.checks {
        assessed.entries[i].visibility_check = Some(check);
        assessed.timings[i].visibility = time;
    }
    let recorded: Vec<&str> = assessed
        .facts
        .iter()
        .flatten()
        .flat_map(|f| f.unprobed.iter().map(|u| u.path.as_str()))
        .collect();
    for (entry, status) in entries.iter().zip(&mut assessed.entries) {
        if status.presence == Presence::Missing {
            let missing = classify_missing_at(entry, cx, &recorded);
            status.clone = Some(missing.clone);
            status.needs_human.extend(missing.needs_human);
        }
    }
    assessed
}

/// Classifies a missing entry (`classify_missing`): its clone held when
/// another entry names its repo, or an unregistered dir clones it, when a
/// live session works at or under its path, or when one of `recorded` —
/// the paths of every probed entry's unprobed worktrees, gone ones among
/// them — is that path.
fn classify_missing_at(entry: &Entry, cx: &Assess<'_>, recorded: &[&str]) -> ClassifiedMissing {
    let path = cx.root.join(&entry.dir);
    classify_missing(
        entry,
        !sessions_under(cx.live, &path).is_empty(),
        recorded.iter().any(|r| same_path(Path::new(r), &path)),
        cx.unregistered,
    )
}

/// A probed repo's checkouts: their paths as its facts spell them — the
/// primary's, each probed worktree's, each unprobed one's — their own git
/// dirs, and their locks. None when the probe found no repo or failed.
pub(crate) fn entry_checkouts(probed: &Probed) -> EntryCheckouts {
    let Probed::Present(facts) = probed else {
        return EntryCheckouts::default();
    };
    EntryCheckouts {
        paths: std::iter::once(&facts.path)
            .chain(facts.worktrees.iter().map(|c| &c.path))
            .chain(facts.unprobed.iter().map(|u| &u.path))
            .cloned()
            .collect(),
        git_dirs: facts.git_dirs.clone(),
        common_dir: Some(facts.common_dir.clone()),
        locks: facts.locks.clone(),
    }
}

/// Assembles an entry's report from its probe and the live sessions in its
/// checkouts, classified for `caller` and `refresh`.
pub fn entry_status(
    entry: &Entry,
    run: ProbeRun,
    sessions: &EntrySessions,
    caller: Caller,
    refresh: Refresh,
) -> EntryStatus {
    let mut status = EntryStatus {
        key: entry.key.clone(),
        kind: entry.kind,
        dir: entry.dir.clone(),
        url: entry.url.to_string(),
        writable: entry.writable,
        archived: entry.archived,
        visibility: entry.visibility,
        ci: entry.ci,
        branch: entry.branch.clone(),
        pinned: entry.pinned,
        refresh: None,
        presence: Presence::Present,
        clone: None,
        layout: None,
        checkouts: Vec::new(),
        branches: Vec::new(),
        stashes: 0,
        fetched_at: None,
        needs_human: Vec::new(),
        probe_error: None,
        unprobed_worktrees: Vec::new(),
        fetch_error: run.fetch.and_then(Result::err),
        visibility_check: None,
    };
    match run.probed {
        Probed::Missing => status.presence = Presence::Missing,
        Probed::NotARepo { detail } => {
            status.presence = Presence::NotARepo;
            status.needs_human.push(NeedsHuman::NotARepo { detail });
        }
        Probed::Failed {
            error,
            layout,
            config,
        } => {
            // a repo is there: what the run asked of it stands, its fetch
            // run or not — held for origin drift once its config was read.
            // Without it the probe never fetched, so a refresh never acts:
            // only a pin's refusal is said
            status.refresh = config.map_or_else(
                || refresh_intent(entry, refresh).filter(|v| *v != RefreshVerdict::Act),
                |config| refresh_verdict(entry, refresh, &config),
            );
            status.probe_error = Some(error);
            status.layout = layout;
        }
        Probed::Present(facts) => {
            status.refresh = refresh_verdict(entry, refresh, &facts.config);
            let classified = classify(entry, &facts, sessions, caller, refresh);
            status.branches = classified.branches;
            status.needs_human = classified.needs_human;
            status.stashes = facts.status.stashes;
            status.fetched_at = facts.fetched_at;
            let primary_busy = sessions.at(&facts.path).to_vec();
            let primary_working = sessions.working_at(&facts.path).to_vec();
            status.checkouts.push(Checkout {
                path: facts.path,
                primary: true,
                head: facts.status.head,
                uncommitted: facts.status.uncommitted,
                in_progress: facts.in_progress,
                locked: facts.primary_locked,
                linked: facts.primary_linked,
                // never removed: not checked
                submodules: None,
                busy: primary_busy,
                working: primary_working,
            });
            status
                .checkouts
                .extend(facts.worktrees.into_iter().map(|mut c| {
                    c.busy = sessions.at(&c.path).to_vec();
                    c.working = sessions.working_at(&c.path).to_vec();
                    c
                }));
            status.unprobed_worktrees = classified.unprobed;
            status.layout = Some(facts.layout);
        }
    }
    status
}

/// Marks each gone worktree that the scan found moved into the workspace
/// root — a stray's `.git` names its git dir — as `Prune::Moved`, naming
/// every such stray: dropping its git dir would orphan them.
pub fn mark_moved_worktrees(entries: &mut [EntryStatus], scan: &Scan) {
    for u in entries.iter_mut().flat_map(|e| &mut e.unprobed_worktrees) {
        let (Some(_), Some(git_dir)) = (&u.prune, &u.worktree.git_dir) else {
            continue;
        };
        let to: Vec<String> = scan
            .unregistered
            .iter()
            .zip(&scan.git_dirs)
            .filter(|(_, g)| g.as_deref() == Some(Path::new(git_dir)))
            .map(|(s, _)| s.dir.clone())
            .collect();
        if !to.is_empty() {
            u.prune = Some(Prune::Moved { to });
        }
    }
}
