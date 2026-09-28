//! `repos status`: probe and classify entries over a bounded thread pool.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::classify::{NeedsHuman, classify};
use crate::git::Git;
use crate::probe::{ProbeContext, ProbeRun, Probed, probe};
use crate::registry::Entry;
use crate::report::EntryStatus;
use crate::state::{Checkout, Presence};

/// How to run `status`.
#[derive(Debug, Clone, Copy)]
pub struct StatusOptions {
    pub fetch: bool,
    /// Entries probed at once; at least one.
    pub jobs: usize,
}

/// One entry's time, for `--timings`.
#[derive(Debug, Clone)]
pub struct EntryTiming {
    pub key: String,
    pub fetch: Duration,
    pub probe: Duration,
}

/// The entries' statuses, in the order given, with their timings.
#[derive(Debug)]
pub struct StatusRun {
    pub entries: Vec<EntryStatus>,
    pub timings: Vec<EntryTiming>,
    /// Wall time of the whole pool.
    pub elapsed: Duration,
}

/// Probes and classifies `entries` over a pool of `opts.jobs` threads.
pub fn status(
    entries: &[Entry],
    root: &Path,
    git: &Git,
    opts: StatusOptions,
    now: SystemTime,
) -> StatusRun {
    let start = Instant::now();
    let cx = ProbeContext {
        git,
        root,
        now,
        fetch: opts.fetch,
    };
    let next = AtomicUsize::new(0);
    let jobs = opts.jobs.clamp(1, entries.len().max(1));
    let mut done: Vec<(usize, EntryStatus, EntryTiming)> = thread::scope(|s| {
        let workers: Vec<_> = (0..jobs)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(entry) = entries.get(i) else { break };
                        let run = probe(entry, cx);
                        let timing = EntryTiming {
                            key: entry.key.clone(),
                            fetch: run.fetch_time,
                            probe: run.probe_time,
                        };
                        out.push((i, entry_status(entry, run, now), timing));
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
    done.sort_by_key(|(i, ..)| *i);
    let (entries, timings) = done.into_iter().map(|(_, e, t)| (e, t)).unzip();
    StatusRun {
        entries,
        timings,
        elapsed: start.elapsed(),
    }
}

/// Assembles an entry's report from its probe.
pub fn entry_status(entry: &Entry, run: ProbeRun, now: SystemTime) -> EntryStatus {
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
        fetched_age_secs: None,
        needs_human: Vec::new(),
        probe_error: None,
        fetch_error: run.fetch.and_then(Result::err),
    };
    match run.probed {
        Probed::Missing => status.presence = Presence::Missing,
        Probed::NotARepo { detail } => {
            status.presence = Presence::NotARepo;
            status.needs_human.push(NeedsHuman::NotARepo { detail });
        }
        Probed::Failed { error } => status.probe_error = Some(error),
        Probed::Present(facts) => {
            let classified = classify(entry, &facts, now);
            status.branches = classified.branches;
            status.needs_human = classified.needs_human;
            status.stashes = facts.status.stashes;
            status.fetched_age_secs = facts.fetched_age_secs;
            status.checkouts.push(Checkout {
                path: facts.path,
                primary: true,
                head: facts.status.head,
                uncommitted: facts.status.uncommitted,
                in_progress: facts.in_progress,
            });
            status.layout = Some(facts.layout);
        }
    }
    status
}
