//! `repos status`: probe entries over a bounded thread pool, scope the live
//! sessions to their checkouts, and classify.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::busy::{EntryCheckouts, EntrySessions, Sessions, scope_sessions};
use crate::classify::{NeedsHuman, classify};
use crate::git::Git;
use crate::probe::{ProbeContext, ProbeRun, Probed, RegistryDirs, probe};
use crate::registry::Entry;
use crate::remote::{VisibilityCheck, is_declared_private, read_anonymously, visibility_url};
use crate::report::EntryStatus;
use crate::scan::Scan;
use crate::sessions::LiveSessions;
use crate::state::{Checkout, Presence, Prune};

/// How to run `status`.
#[derive(Debug, Clone, Copy)]
pub struct StatusOptions<'a> {
    /// Fetch owned, non-pinned entries from `origin` before probing, and run
    /// the visibility check on each `[repos]` entry declared private.
    pub fetch: bool,
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
    Entry(usize, Box<ProbeRun>, EntryTiming),
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
    let cx = ProbeContext {
        git,
        root,
        registry_dirs,
        fetch: opts.fetch,
    };
    // the entries the visibility check reads, by index
    let checks: Vec<usize> = if opts.fetch {
        (0..entries.len())
            .filter(|&i| is_declared_private(&entries[i]))
            .collect()
    } else {
        Vec::new()
    };
    let tasks = checks.len() + entries.len();
    let next = AtomicUsize::new(0);
    let jobs = opts.jobs.clamp(1, tasks.max(1));
    let done: Vec<Done> = thread::scope(|s| {
        let workers: Vec<_> = (0..jobs)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let task = next.fetch_add(1, Ordering::Relaxed);
                        if let Some(&i) = checks.get(task) {
                            let start = Instant::now();
                            let url = visibility_url(&entries[i], opts.visibility_base);
                            let check = read_anonymously(git, root, &url);
                            out.push(Done::Visibility(i, check, start.elapsed()));
                            continue;
                        }
                        let i = task - checks.len();
                        let Some(entry) = entries.get(i) else { break };
                        let run = probe(entry, cx);
                        let timing = EntryTiming {
                            key: entry.key.clone(),
                            fetch: run.fetch_time,
                            probe: run.probe_time,
                            visibility: Duration::ZERO,
                        };
                        out.push(Done::Entry(i, Box::new(run), timing));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            // a worker only panics on a bug; surface it rather than drop entries
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    let mut probed = Vec::with_capacity(entries.len());
    let mut checked = Vec::with_capacity(checks.len());
    for d in done {
        match d {
            Done::Entry(i, run, timing) => probed.push((i, run, timing)),
            Done::Visibility(i, check, time) => checked.push((i, check, time)),
        }
    }
    probed.sort_by_key(|(i, ..)| *i);
    // every checkout probed, so a session lands in the deepest of them all
    let checkouts: Vec<EntryCheckouts> = probed
        .iter()
        .map(|(_, run, _)| entry_checkouts(&run.probed))
        .collect();
    let (sessions, per_entry) = scope_sessions(opts.live, &checkouts);
    let mut statuses: Vec<(usize, EntryStatus, EntryTiming)> = probed
        .into_iter()
        .zip(&per_entry)
        .map(|((i, run, timing), busy)| (i, entry_status(&entries[i], *run, busy), timing))
        .collect();
    for (i, check, time) in checked {
        if let Some((_, status, timing)) = statuses.iter_mut().find(|(j, ..)| *j == i) {
            status.visibility_check = Some(check);
            timing.visibility = time;
        }
    }
    let (entries, timings) = statuses.into_iter().map(|(_, e, t)| (e, t)).unzip();
    StatusRun {
        entries,
        sessions,
        timings,
        elapsed: start.elapsed(),
    }
}

/// A probed repo's checkouts: their paths as its facts spell them — the
/// primary's, each probed worktree's, each unprobed one's — their own git
/// dirs, and their locks. None when the probe found no repo or failed.
fn entry_checkouts(probed: &Probed) -> EntryCheckouts {
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
/// checkouts.
pub fn entry_status(entry: &Entry, run: ProbeRun, sessions: &EntrySessions) -> EntryStatus {
    let mut status = EntryStatus {
        key: entry.key.clone(),
        kind: entry.kind,
        dir: entry.dir.clone(),
        url: entry.url.to_string(),
        writable: entry.writable,
        archived: entry.archived,
        visibility: entry.visibility,
        ci: entry.ci,
        checkout_mode: entry.checkout_mode.clone(),
        presence: Presence::Present,
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
        Probed::Failed { error, layout } => {
            status.probe_error = Some(error);
            status.layout = layout;
        }
        Probed::Present(facts) => {
            let classified = classify(entry, &facts, sessions);
            status.branches = classified.branches;
            status.needs_human = classified.needs_human;
            status.stashes = facts.status.stashes;
            status.fetched_at = facts.fetched_at;
            let primary_busy = sessions.at(&facts.path).to_vec();
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
            });
            status
                .checkouts
                .extend(facts.worktrees.into_iter().map(|mut c| {
                    c.busy = sessions.at(&c.path).to_vec();
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
