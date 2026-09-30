//! The `--json` contract's golden fixtures: hand-built documents, serialized
//! and compared with the JSON checked in at the repo root's
//! `src/test/fixtures/repos_status/`, where the TS side reads them.
//!
//! Compared as parsed JSON, so a formatter's reflow of a checked-in file
//! can't fail the test. Never hand-edit the files: regenerate them with
//! `UPDATE_GOLDEN=1 cargo test --test golden`, and bump
//! `STATUS_FORMAT_VERSION` (and with it `SYNC_FORMAT_VERSION`, whose document
//! embeds the status report) when the change breaks the shape; a change to
//! the sync document alone bumps `SYNC_FORMAT_VERSION`.
//!
//! Between them the documents cover every variant of the report's enums
//! (`sessions.json` lists every state of busy detection, which a report
//! carries one of),
//! each domain's built in its own function so that an every-variant coverage
//! floor (an exhaustive `match` per enum that fails to compile when a
//! variant lands uncovered) can attach beside it to keep it so.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use fuz_repos::busy::Sessions;
use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::error::Error;
use fuz_repos::registry::{CheckoutList, EntryKind, EntryName, RegistryIssue, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    BranchOutcome, BranchSync, CloneOutcome, EntryStatus, EntrySync, ErrorReport, FetchOutcome,
    RepairBlock, StatusReport, SyncHold, SyncReport, UnregisteredClone, UnregisteredKind,
};
use fuz_repos::sessions::{Session, SessionSource, Unavailable};
use fuz_repos::state::{
    BranchNeedsHuman, BranchStatus, Checkout, CleanupReason, CloneRecipe, CloneVerdict,
    GitDirHolds, Head, HeldBy, InProgressOp, Layout, Presence, Prune, PruneLoss, RefreshVerdict,
    Relation, SyncAction, Uncommitted, UnprobedHead, UnprobedWhy, UnprobedWorktree,
    UnprobedWorktreeStatus, Verdict,
};
use fuz_repos::{STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use serde::Serialize;
use serde_json::Value;

/// The fixed clock every timestamp is built from.
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

const WORKSPACE: &str = "/home/me/dev";

/// `src/test/fixtures/repos_status/` under the repo root, two above the
/// crate.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("src/test/fixtures/repos_status")
}

/// Compares `doc` with the fixture `name`, or rewrites the fixture under
/// `UPDATE_GOLDEN` (tab-indented, as the repo's other JSON is).
fn assert_golden(name: &str, doc: &impl Serialize) {
    let path = fixtures_dir().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| !v.is_empty()) {
        let mut buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        doc.serialize(&mut ser).unwrap();
        buf.push(b'\n');
        std::fs::create_dir_all(fixtures_dir()).unwrap();
        std::fs::write(&path, buf).unwrap();
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — generate it with `UPDATE_GOLDEN=1 cargo test --test golden`",
            path.display()
        )
    });
    let golden: Value = serde_json::from_str(&text).unwrap();
    let actual = serde_json::to_value(doc).unwrap();
    if let Some(at) = first_difference(&golden, &actual, String::new()) {
        panic!(
            "{} drifted from the serialized document at `{at}`: if the change is intended, \
             regenerate with `UPDATE_GOLDEN=1 cargo test --test golden`, and bump STATUS_FORMAT_VERSION or SYNC_FORMAT_VERSION if it breaks the shape",
            path.display()
        );
    }
}

/// The JSON pointer of the first place `a` and `b` differ.
fn first_difference(a: &Value, b: &Value, at: String) -> Option<String> {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter().find_map(|k| match (a.get(k), b.get(k)) {
                (Some(x), Some(y)) => first_difference(x, y, format!("{at}/{k}")),
                _ => Some(format!("{at}/{k}")),
            })
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (x, y))| first_difference(x, y, format!("{at}/{i}"))),
        _ => (a != b).then_some(at),
    }
}

#[test]
fn status_report() {
    let doc = status_report_doc();
    assert_eq!(doc.version, STATUS_FORMAT_VERSION);
    assert!(doc.fetched);
    // every clone verdict: exhaustive, so a new one fails to compile here
    let kinds: std::collections::BTreeSet<usize> = doc
        .entries
        .iter()
        .filter_map(|e| e.clone.as_ref())
        .map(|c| match c {
            CloneVerdict::Act { .. } => 0,
            CloneVerdict::Held { .. } => 1,
        })
        .collect();
    assert_eq!(kinds, (0..2).collect());
    // a verdict exactly when missing
    assert!(
        doc.entries
            .iter()
            .all(|e| e.clone.is_some() == (e.presence == Presence::Missing))
    );
    assert_golden("status_report.json", &doc);
}

#[test]
fn status_report_targeted() {
    let doc = targeted_doc();
    // targets narrowed the run: no scan, `null` — never `[]`, which reads as
    // "none found"
    assert!(doc.unregistered.is_none());
    // no `--fetch`: no entry was fetched or checked
    assert!(!doc.fetched);
    assert!(
        doc.entries
            .iter()
            .all(|e| e.fetch_error.is_none() && e.visibility_check.is_none())
    );
    // every refresh verdict: exhaustive, so a new one fails to compile here
    let kinds: std::collections::BTreeSet<usize> = doc
        .entries
        .iter()
        .filter_map(|e| e.refresh.as_ref())
        .map(|r| match r {
            RefreshVerdict::Act => 0,
            RefreshVerdict::Held { .. } => 1,
        })
        .collect();
    assert_eq!(kinds, (0..2).collect());
    assert_golden("status_report_targeted.json", &doc);
}

#[test]
fn sessions_states() {
    assert_golden("sessions.json", &sessions_doc());
}

#[test]
fn sync_report() {
    let doc = sync_report_doc();
    assert_eq!(doc.version, SYNC_FORMAT_VERSION);
    assert_eq!(doc.status.version, STATUS_FORMAT_VERSION);
    assert!(doc.status.fetched);
    // one outcome per status entry and branch, in order
    assert_eq!(doc.entries.len(), doc.status.entries.len());
    for (e, s) in doc.status.entries.iter().zip(&doc.entries) {
        assert_eq!(e.key, s.key);
        // a clone outcome exactly for a clone verdict
        assert_eq!(e.clone.is_some(), s.clone.is_some(), "{}", e.key);
        let names = |b: &[BranchStatus]| b.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(&e.branches),
            s.branches
                .iter()
                .map(|b| b.name.clone())
                .collect::<Vec<_>>()
        );
    }
    // a failed action, fetch, and probe: exit 1
    assert!(doc.failed());
    // `--references`: each third-party reference present refreshed, its
    // refresh carried out as its fetch and its branches' outcomes
    for (e, s) in doc.status.entries.iter().zip(&doc.entries) {
        let refreshed = !e.writable && !e.pinned && e.presence == Presence::Present;
        assert_eq!(
            e.refresh == Some(RefreshVerdict::Act),
            refreshed,
            "{}",
            e.key
        );
        if refreshed {
            assert_eq!(s.fetch, FetchOutcome::Fetched, "{}", e.key);
        }
    }
    assert_sync_coverage(&doc);
    assert_golden("sync_report.json", &doc);
}

/// The sync document's every-variant floor: each outcome, fetch outcome,
/// and hold appears at least once. The matches are exhaustive, so a new
/// variant fails to compile here until the document covers it.
fn assert_sync_coverage(doc: &SyncReport) {
    let outcome = |o: &BranchOutcome| match o {
        BranchOutcome::Untouched => 0,
        BranchOutcome::NeedsHuman { .. } => 1,
        BranchOutcome::Held { .. } => 2,
        BranchOutcome::FastForwarded { .. } => 3,
        BranchOutcome::Moved { .. } => 4,
        BranchOutcome::Pushed { .. } => 5,
        BranchOutcome::PushFailed { .. } => 6,
        BranchOutcome::Failed { .. } => 7,
    };
    let fetch = |f: &FetchOutcome| match f {
        FetchOutcome::Fetched => 0,
        FetchOutcome::Failed { .. } => 1,
        FetchOutcome::NotFetched => 2,
    };
    let hold = |h: SyncHold| match h {
        SyncHold::Pinned => 0,
        SyncHold::Entry => 1,
        SyncHold::PushUrl => 2,
        SyncHold::FetchFailed => 3,
        SyncHold::DirtyCheckout => 4,
        SyncHold::UnprobedWorktree => 5,
        SyncHold::SeveralCheckouts => 6,
        SyncHold::Busy => 7,
        SyncHold::BusyUnknown => 8,
        SyncHold::Gateway => 9,
        SyncHold::Changed => 10,
        // a refresh's hold alone, never a branch's: the targeted status
        // document carries it, as a refresh verdict's
        SyncHold::OriginNotHttps => 11,
    };
    let clone = |c: &CloneOutcome| match c {
        CloneOutcome::Cloned { .. } => 0,
        CloneOutcome::Held { .. } => 1,
        CloneOutcome::CloneFailed { .. } => 2,
        CloneOutcome::Failed { .. } => 3,
    };
    let branches = || doc.entries.iter().flat_map(|e| &e.branches);
    let seen = |ids: Vec<usize>| ids.into_iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        seen(
            doc.entries
                .iter()
                .filter_map(|e| e.clone.as_ref().map(clone))
                .collect()
        ),
        (0..4).collect()
    );
    assert_eq!(
        seen(branches().map(|b| outcome(&b.outcome)).collect()),
        (0..8).collect()
    );
    assert_eq!(
        seen(doc.entries.iter().map(|e| fetch(&e.fetch)).collect()),
        (0..3).collect()
    );
    assert_eq!(
        seen(
            branches()
                .filter_map(|b| match b.outcome {
                    BranchOutcome::Held { by, .. } => Some(hold(by)),
                    _ => None,
                })
                .collect()
        ),
        (0..11).collect()
    );
}

#[test]
fn error_report() {
    let doc = error_doc();
    assert_eq!(doc.version, STATUS_FORMAT_VERSION);
    assert_golden("error_report.json", &doc);
}

/// A fatal error under `sync --json`: the same document, at the sync
/// report's version, which is how a consumer knows which command it came
/// from.
#[test]
fn sync_error_report() {
    let doc = ErrorReport::new(
        &Error::UnknownEntry {
            name: "fuz_ap".into(),
            suggestions: vec!["fuz_app".into(), "fuz_css".into()],
        },
        SYNC_FORMAT_VERSION,
    );
    assert_eq!(doc.version, SYNC_FORMAT_VERSION);
    assert_golden("sync_error_report.json", &doc);
}

#[test]
fn a_drifted_golden_names_where() {
    let a = serde_json::json!({"entries": [{"key": "app", "stashes": 0}], "version": 3});
    let b = serde_json::json!({"entries": [{"key": "app", "stashes": 1}], "version": 3});
    assert_eq!(
        first_difference(&a, &b, String::new()).as_deref(),
        Some("/entries/0/stashes")
    );
    assert_eq!(first_difference(&a, &a, String::new()), None);
    let c = serde_json::json!({"entries": [], "version": 3});
    assert_eq!(
        first_difference(&a, &c, String::new()).as_deref(),
        Some("/entries")
    );
    let d = serde_json::json!({"entries": [{"key": "app", "stashes": 0}]});
    assert_eq!(
        first_difference(&a, &d, String::new()).as_deref(),
        Some("/version")
    );
}

// --- the documents ---

/// A whole-workspace run under `--fetch`: every entry shape, every fetch
/// failure and visibility check, and the unregistered scan.
fn status_report_doc() -> StatusReport {
    let mut report = StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        true,
        Sessions::Available {
            unscoped: unscoped_sessions(),
        },
        vec![
            app(),
            blog(),
            archived(),
            test262(),
            spec(),
            zzz(),
            missing(),
            // a session works where its dir was deleted
            missing_reference("wpt", Some(HeldBy::Busy)),
            twin(),
            renamed(),
            not_a_repo(),
            partial(),
            forge(),
            gro(),
            zap(),
            site(),
            mdz(),
            tsv(),
            tsv_fuz_dev(),
            fuz_css(),
            fuz_ui(),
            uz(),
            pushy(),
            agent_run(),
        ],
    );
    report.unregistered = Some(unregistered());
    report
}

/// A run narrowed by targets, local refs only: the scan, the fetch, and
/// the visibility check didn't run; busy detection was unavailable, so
/// every action is held. The targets name every entry: a third-party
/// reference among them is refreshed, a pin refused.
fn targeted_doc() -> StatusReport {
    StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        false,
        Sessions::Unavailable {
            reason: foreign_domain(),
        },
        vec![
            EntryStatus {
                needs_human: vec![NeedsHuman::OriginMismatch {
                    origin: OriginRemote::Missing,
                    expected: "git@github.com:me/gro".into(),
                    fix: OriginFix::Add,
                }],
                ..entry("gro", Some("main"))
            },
            EntryStatus {
                branches: vec![BranchStatus {
                    unique_commits: 1,
                    worktree: Some(path("fuz_util")),
                    ..branch(
                        "main",
                        Some("origin/main"),
                        Relation::Ahead { commits: 1 },
                        Verdict::Held {
                            action: SyncAction::Push { commits: 1 },
                            by: HeldBy::BusyUnknown,
                        },
                    )
                }],
                ..entry("fuz_util", Some("main"))
            },
            // named: refreshed, previewed from local refs
            EntryStatus {
                kind: EntryKind::Reference,
                url: "https://github.com/them/typescript".into(),
                writable: false,
                visibility: None,
                ci: false,
                refresh: Some(RefreshVerdict::Act),
                branches: vec![branch(
                    "main",
                    Some("origin/main"),
                    Relation::Behind { commits: 3 },
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 3 },
                        by: HeldBy::BusyUnknown,
                    },
                )],
                ..entry("typescript", Some("main"))
            },
            // named, its origin the repo over SSH: held, never fetched
            EntryStatus {
                kind: EntryKind::Reference,
                url: "https://github.com/them/lit".into(),
                writable: false,
                visibility: None,
                ci: false,
                refresh: Some(RefreshVerdict::Held {
                    by: HeldBy::OriginNotHttps,
                }),
                needs_human: vec![NeedsHuman::OriginNotHttps {
                    fetch_url: "git@github.com:them/lit".into(),
                    expected: "https://github.com/them/lit".into(),
                    fix: Some(OriginFix::SetUrl),
                }],
                ..entry("lit", Some("main"))
            },
            // named, an `insteadOf` rewriting its HTTPS origin to SSH
            EntryStatus {
                kind: EntryKind::Reference,
                url: "https://github.com/them/dom".into(),
                writable: false,
                visibility: None,
                ci: false,
                refresh: Some(RefreshVerdict::Held {
                    by: HeldBy::OriginNotHttps,
                }),
                needs_human: vec![NeedsHuman::OriginNotHttps {
                    fetch_url: "git@github.com:them/dom".into(),
                    expected: "https://github.com/them/dom".into(),
                    fix: None,
                }],
                ..entry("dom", Some("main"))
            },
            // named: a pin refuses
            EntryStatus {
                kind: EntryKind::Reference,
                visibility: None,
                ci: false,
                pinned: true,
                refresh: Some(RefreshVerdict::Held { by: HeldBy::Pinned }),
                checkouts: vec![primary("wpt", on("fork"))],
                branches: vec![branch(
                    "fork",
                    Some("origin/fork"),
                    Relation::Behind { commits: 2 },
                    Verdict::Held {
                        action: SyncAction::FastForward { commits: 2 },
                        by: HeldBy::Pinned,
                    },
                )],
                ..entry("wpt", Some("fork"))
            },
        ],
    )
}

/// Every state of busy detection, one per report.
fn sessions_doc() -> Vec<Sessions> {
    vec![
        Sessions::Available {
            unscoped: unscoped_sessions(),
        },
        Sessions::Available { unscoped: vec![] },
        Sessions::Unavailable {
            reason: Unavailable::HomeUnknown,
        },
        Sessions::Unavailable {
            reason: Unavailable::RelativeConfigDir {
                path: "claude".into(),
            },
        },
        Sessions::Unavailable {
            reason: Unavailable::Unreadable {
                path: "/home/me/.claude/sessions".into(),
                error: "Permission denied (os error 13)".into(),
            },
        },
        Sessions::Unavailable {
            reason: Unavailable::Unparseable {
                path: "/home/me/.claude/sessions/4242.json".into(),
                error: "missing field `procStart` at line 1 column 80".into(),
            },
        },
        Sessions::Unavailable {
            reason: foreign_domain(),
        },
    ]
}

fn foreign_domain() -> Unavailable {
    Unavailable::ForeignPidDomain {
        path: "/home/me/.claude/sessions/77.json".into(),
        pid_domain: "linux:0123456789abcdef0123456789abcdef:pid:[4026532001]".into(),
        source: SessionSource::SessionFile,
    }
}

/// Live sessions in no checkout: one at the workspace root, a background
/// worker outside it.
fn unscoped_sessions() -> Vec<Session> {
    vec![
        Session::at(1200, 0, WORKSPACE.into(), SessionSource::SessionFile),
        Session::at(
            1300,
            0,
            "/home/me/notes".into(),
            SessionSource::RosterWorker,
        ),
    ]
}

/// A `sync --references` run over the whole workspace: every outcome,
/// fetch outcome, and hold, each branch's verdict the one the outcome
/// carries out.
fn sync_report_doc() -> SyncReport {
    let ff = |commits| SyncAction::FastForward { commits };
    let push = |commits| SyncAction::Push { commits };
    let act = |action| Verdict::Act { action };
    let held_by = |action, by| Verdict::Held { action, by };
    let behind = |commits| Relation::Behind { commits };
    let up = |name: &str| format!("origin/{name}");
    let oid = |c: char| c.to_string().repeat(40);
    // (name, relation, verdict, outcome)
    let app_branches: Vec<(&str, Relation, Verdict, BranchOutcome)> = vec![
        (
            "main",
            behind(2),
            act(ff(2)),
            BranchOutcome::FastForwarded {
                from: oid('a'),
                to: oid('b'),
            },
        ),
        (
            "docs",
            Relation::Shallow,
            act(SyncAction::Move),
            BranchOutcome::Moved {
                from: oid('c'),
                to: oid('d'),
            },
        ),
        (
            "feat",
            behind(1),
            act(ff(1)),
            BranchOutcome::Failed {
                action: ff(1),
                message: "git rejected moving refs/heads/feat: not a fast-forward".into(),
            },
        ),
        (
            "late",
            behind(1),
            act(ff(1)),
            BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::Changed,
            },
        ),
        (
            "ahead",
            Relation::Ahead { commits: 3 },
            act(push(3)),
            BranchOutcome::Pushed {
                from: Some(oid('e')),
                to: oid('f'),
            },
        ),
        // deleted on the remote after the fetch: the push made it anew
        (
            "fresh",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            BranchOutcome::Pushed {
                from: None,
                to: oid('9'),
            },
        ),
        (
            "guarded",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            BranchOutcome::PushFailed {
                failure: RemoteFailure::Rejected {
                    reason: "protected branch hook declined".into(),
                    message: Some(
                        "GH006: Protected branch update failed for refs/heads/guarded.".into(),
                    ),
                },
            },
        ),
        // a push URL set between classifying and pushing
        (
            "rerouted",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            BranchOutcome::Held {
                action: push(1),
                by: SyncHold::PushUrl,
            },
        ),
        (
            "agent",
            Relation::Ahead { commits: 2 },
            held_by(push(2), HeldBy::Gateway),
            BranchOutcome::Held {
                action: push(2),
                by: SyncHold::Gateway,
            },
        ),
        (
            "dirty",
            behind(1),
            held_by(ff(1), HeldBy::DirtyCheckout),
            BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::DirtyCheckout,
            },
        ),
        (
            "busy",
            behind(1),
            held_by(ff(1), HeldBy::Busy),
            BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::Busy,
            },
        ),
        (
            "unseen",
            behind(1),
            act(ff(1)),
            BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::BusyUnknown,
            },
        ),
        (
            "usb",
            behind(4),
            held_by(ff(4), HeldBy::UnprobedWorktree),
            BranchOutcome::Held {
                action: ff(4),
                by: SyncHold::UnprobedWorktree,
            },
        ),
        (
            "twin",
            behind(1),
            held_by(ff(1), HeldBy::SeveralCheckouts),
            BranchOutcome::Held {
                action: ff(1),
                by: SyncHold::SeveralCheckouts,
            },
        ),
        (
            "arc",
            Relation::Diverged {
                ahead: 1,
                behind: 1,
            },
            Verdict::NeedsHuman {
                reason: BranchNeedsHuman::Diverged,
            },
            BranchOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Diverged,
            },
        ),
        (
            "done",
            Relation::InSync,
            Verdict::Quiet,
            BranchOutcome::Untouched,
        ),
    ];
    let mut app = entry("app", Some("main"));
    let mut app_sync = Vec::new();
    for (name, relation, verdict, outcome) in app_branches {
        app.branches
            .push(branch(name, Some(&up(name)), relation, verdict));
        app_sync.push(BranchSync {
            name: name.into(),
            outcome,
            repeats: None,
        });
    }
    // a linked worktree of app's, its own entry: app acted on `main` for
    // the repo
    let app_wt = EntryStatus {
        dir: "app-wt".into(),
        url: "https://github.com/me/app".into(),
        checkouts: vec![primary("app-wt", on("wt"))],
        branches: vec![branch("main", Some("origin/main"), behind(2), act(ff(2)))],
        ..entry("app_wt", Some("main"))
    };
    let app_wt_sync = vec![BranchSync {
        name: "main".into(),
        outcome: BranchOutcome::FastForwarded {
            from: oid('a'),
            to: oid('b'),
        },
        repeats: Some("app".into()),
    }];
    let one = |e: &EntryStatus, relation, verdict, outcome| {
        let mut e = e.clone();
        e.branches = vec![branch("main", Some("origin/main"), relation, verdict)];
        let sync = vec![BranchSync {
            name: "main".into(),
            outcome,
            repeats: None,
        }];
        (e, sync)
    };
    let (blog, blog_sync) = one(
        &EntryStatus {
            needs_human: vec![NeedsHuman::OperationInProgress {
                checkout: path("blog"),
                op: InProgressOp::Rebase,
            }],
            ..entry("blog", Some("main"))
        },
        behind(1),
        held_by(ff(1), HeldBy::Entry),
        BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Entry,
        },
    );
    let forge_failure = RemoteFailure::Unreachable {
        cause: UnreachableCause::Auth,
        message: "git@github.com: Permission denied (publickey).".into(),
    };
    let (forge, forge_sync) = one(
        &EntryStatus {
            fetch_error: Some(forge_failure.clone()),
            ..entry("fuz_forge", Some("main"))
        },
        behind(2),
        held_by(ff(2), HeldBy::FetchFailed),
        BranchOutcome::Held {
            action: ff(2),
            by: SyncHold::FetchFailed,
        },
    );
    let (spec, spec_sync) = one(
        &EntryStatus {
            kind: EntryKind::Reference,
            branch: None,
            pinned: true,
            ..entry("spec", None)
        },
        behind(1),
        held_by(ff(1), HeldBy::Pinned),
        BranchOutcome::Held {
            action: ff(1),
            by: SyncHold::Pinned,
        },
    );
    // `--references`: fetched over HTTPS, fast-forwarded where clean;
    // never pushed, so a branch ahead is local-only work
    let lib = EntryStatus {
        kind: EntryKind::Reference,
        url: "https://github.com/them/lib".into(),
        writable: false,
        visibility: None,
        ci: false,
        branch: None,
        refresh: Some(RefreshVerdict::Act),
        branches: vec![
            branch("main", Some("origin/main"), behind(1), act(ff(1))),
            BranchStatus {
                unique_commits: 2,
                ..branch(
                    "audit",
                    Some("origin/audit"),
                    Relation::Ahead { commits: 2 },
                    Verdict::LocalOnly,
                )
            },
        ],
        ..entry("lib", None)
    };
    let lib_sync = vec![
        BranchSync {
            name: "main".into(),
            outcome: BranchOutcome::FastForwarded {
                from: oid('5'),
                to: oid('6'),
            },
            repeats: None,
        },
        BranchSync {
            name: "audit".into(),
            outcome: BranchOutcome::Untouched,
            repeats: None,
        },
    ];
    let broken = EntryStatus {
        probe_error: Some("git status failed (128): error: bad tree object HEAD".into()),
        checkouts: vec![],
        ..entry("broken", Some("main"))
    };
    // missing: cloned, held as classified or at the moment of cloning,
    // failed at the remote, failed placing it
    let stray = EntryStatus {
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/stray".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: HeldBy::UnprobedWorktree,
        }),
        ..missing()
    };
    let stray = EntryStatus {
        key: "stray".into(),
        dir: "stray".into(),
        url: "https://github.com/me/stray".into(),
        ..stray
    };
    let mut status = StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        true,
        Sessions::Available { unscoped: vec![] },
        vec![
            app,
            app_wt,
            blog,
            forge,
            spec,
            lib,
            missing(),
            stray,
            twin(),
            renamed(),
            missing_reference("wpt", None),
            missing_reference("html", None),
            missing_reference("dom", None),
            broken,
        ],
    );
    // no targets: the scan ran first
    status.unregistered = Some(vec![renamed_old()]);
    let sync = |key: &str, fetch, branches| EntrySync {
        key: key.into(),
        fetch,
        clone: None,
        branches,
    };
    let cloned = |key: &str, clone| EntrySync {
        key: key.into(),
        fetch: FetchOutcome::NotFetched,
        clone: Some(clone),
        branches: vec![],
    };
    SyncReport::new(
        status,
        vec![
            sync("app", FetchOutcome::Fetched, app_sync),
            sync("app_wt", FetchOutcome::Fetched, app_wt_sync),
            sync("blog", FetchOutcome::Fetched, blog_sync),
            sync(
                "fuz_forge",
                FetchOutcome::Failed {
                    failure: forge_failure,
                },
                forge_sync,
            ),
            sync("spec", FetchOutcome::NotFetched, spec_sync),
            sync("lib", FetchOutcome::Fetched, lib_sync),
            cloned(
                "blake3",
                CloneOutcome::Cloned {
                    branch: "main".into(),
                    head: oid('c'),
                },
            ),
            cloned(
                "stray",
                CloneOutcome::Held {
                    by: SyncHold::UnprobedWorktree,
                },
            ),
            cloned(
                "twin",
                CloneOutcome::Held {
                    by: SyncHold::Entry,
                },
            ),
            cloned(
                "renamed",
                CloneOutcome::Held {
                    by: SyncHold::Entry,
                },
            ),
            // a dir made at the path since the probe
            cloned(
                "wpt",
                CloneOutcome::Held {
                    by: SyncHold::Changed,
                },
            ),
            cloned(
                "html",
                CloneOutcome::CloneFailed {
                    failure: RemoteFailure::RepoNotFound {
                        message: "remote: Repository not found.".into(),
                    },
                },
            ),
            cloned(
                "dom",
                CloneOutcome::Failed {
                    message: format!(
                        "can't move the clone into {WORKSPACE}/dom: Permission denied (os \
                         error 13)"
                    ),
                },
            ),
            sync("broken", FetchOutcome::Fetched, vec![]),
        ],
    )
}

/// A fatal error under `--json`, the kind with the richest payload.
fn error_doc() -> ErrorReport {
    ErrorReport::new(
        &Error::RegistryInvalid {
            path: PathBuf::from(format!("{WORKSPACE}/repos.toml")),
            issues: registry_issues(),
        },
        STATUS_FORMAT_VERSION,
    )
}

// --- entries ---

fn path(dir: &str) -> String {
    format!("{WORKSPACE}/{dir}")
}

const fn plain_layout() -> Layout {
    Layout {
        shallow: false,
        sparse: false,
        partial_filter: None,
    }
}

fn primary(dir: &str, head: Head) -> Checkout {
    Checkout {
        path: path(dir),
        primary: true,
        head,
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: false,
        submodules: None,
        busy: vec![],
    }
}

fn on(name: &str) -> Head {
    Head::Branch { name: name.into() }
}

/// A present, owned, public repo on `main`, unpinned, fetched a day ago,
/// with nothing to say.
fn entry(key: &str, branch: Option<&str>) -> EntryStatus {
    EntryStatus {
        key: key.into(),
        kind: EntryKind::Repo,
        dir: key.into(),
        url: format!("https://github.com/me/{key}"),
        writable: true,
        archived: false,
        visibility: Some(Visibility::Public),
        ci: true,
        branch: branch.map(str::to_owned),
        pinned: false,
        refresh: None,
        presence: Presence::Present,
        clone: None,
        layout: Some(plain_layout()),
        checkouts: vec![primary(key, on("main"))],
        branches: vec![],
        stashes: 0,
        fetched_at: Some(NOW - DAY),
        needs_human: vec![],
        probe_error: None,
        unprobed_worktrees: vec![],
        fetch_error: None,
        visibility_check: None,
    }
}

fn branch(
    name: &str,
    upstream: Option<&str>,
    relation: Relation,
    verdict: Verdict,
) -> BranchStatus {
    BranchStatus {
        name: name.into(),
        upstream: upstream.map(str::to_owned),
        worktree: None,
        symref: None,
        unique_commits: 0,
        newest_commit_at: NOW - 2 * DAY,
        relation,
        verdict,
    }
}

/// The busiest repo: every relation but `Shallow` (`test262()`), every
/// verdict kind, every `HeldBy` but `Entry` (`blog()`), `Pinned` (`spec()`),
/// `FetchFailed` (`forge()`), `SeveralCheckouts` (`gro()`), and
/// `BusyUnknown` (the targeted document), linked worktrees — one a live
/// session works in — and every way a worktree goes unprobed, one busy too.
fn app() -> EntryStatus {
    let mut e = entry("app", Some("main"));
    e.fetch_error = Some(RemoteFailure::Failed {
        message: "fatal: protocol error: bad line length character: Welc".into(),
    });
    e.stashes = 2;
    e.checkouts[0].uncommitted = Uncommitted {
        staged: 1,
        unstaged: 2,
        untracked: 3,
        conflicted: 0,
    };
    e.checkouts.push(Checkout {
        path: path("app-feat"),
        primary: false,
        head: on("feat"),
        uncommitted: Uncommitted {
            staged: 0,
            unstaged: 1,
            untracked: 0,
            conflicted: 0,
        },
        in_progress: None,
        locked: false,
        linked: true,
        submodules: None,
        busy: vec![],
    });
    e.checkouts.push(Checkout {
        path: path("app/.claude/worktrees/agent"),
        primary: false,
        head: on("agent"),
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: None,
        // launched at the workspace root, its process since moved in
        busy: vec![Session {
            process_cwd: Some(path("app/.claude/worktrees/agent/src")),
            ..Session::at(4242, 0, WORKSPACE.into(), SessionSource::SessionFile)
        }],
    });
    e.checkouts.push(Checkout {
        path: "/home/me/wt/app-old".into(),
        primary: false,
        head: on("old"),
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: Some(false),
        busy: vec![],
    });
    e.checkouts.push(Checkout {
        path: path("app-fix"),
        primary: false,
        head: Head::Detached {
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
        },
        uncommitted: Uncommitted {
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 1,
        },
        in_progress: Some(InProgressOp::CherryPick),
        locked: true,
        linked: true,
        submodules: Some(true),
        busy: vec![],
    });
    e.branches = vec![
        BranchStatus {
            unique_commits: 2,
            worktree: Some(path("app")),
            ..branch(
                "main",
                Some("origin/main"),
                Relation::Ahead { commits: 2 },
                Verdict::Act {
                    action: SyncAction::Push { commits: 2 },
                },
            )
        },
        BranchStatus {
            worktree: Some(path("app-feat")),
            ..branch(
                "feat",
                Some("origin/feat"),
                Relation::Behind { commits: 1 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: HeldBy::DirtyCheckout,
                },
            )
        },
        BranchStatus {
            unique_commits: 1,
            worktree: Some(path("app/.claude/worktrees/agent")),
            ..branch(
                "agent",
                Some("origin/agent"),
                Relation::Ahead { commits: 1 },
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: HeldBy::Busy,
                },
            )
        },
        branch(
            "usb",
            Some("origin/usb"),
            Relation::Behind { commits: 4 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 4 },
                by: HeldBy::UnprobedWorktree,
            },
        ),
        BranchStatus {
            unique_commits: 2,
            ..branch(
                "arc",
                Some("origin/arc"),
                Relation::Diverged {
                    ahead: 2,
                    behind: 5,
                },
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            )
        },
        BranchStatus {
            unique_commits: 3,
            ..branch(
                "fork",
                Some("origin/fork"),
                Relation::Unmapped,
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Unmapped,
                },
            )
        },
        BranchStatus {
            unique_commits: 1,
            newest_commit_at: NOW - 40 * DAY,
            ..branch("wip", None, Relation::Untracked, Verdict::LocalOnly)
        },
        BranchStatus {
            worktree: Some("/home/me/wt/app-old".into()),
            ..branch(
                "old",
                Some("origin/old"),
                Relation::Gone,
                Verdict::Cleanup {
                    reason: CleanupReason::UpstreamGone,
                    removable_worktree: Some("/home/me/wt/app-old".into()),
                },
            )
        },
        branch(
            "done",
            None,
            Relation::Untracked,
            Verdict::Cleanup {
                reason: CleanupReason::Merged,
                removable_worktree: None,
            },
        ),
        branch(
            "synced",
            Some("origin/synced"),
            Relation::InSync,
            Verdict::Quiet,
        ),
        branch(
            "theirs",
            Some("upstream/main"),
            Relation::Untracked,
            Verdict::Quiet,
        ),
        // an alias of `main`: never acted on
        BranchStatus {
            symref: Some("refs/heads/main".into()),
            ..branch("m", None, Relation::Untracked, Verdict::Quiet)
        },
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("app-fix"),
            op: InProgressOp::CherryPick,
        },
        NeedsHuman::OperationInProgress {
            checkout: "/media/usb/app".into(),
            op: InProgressOp::Bisect,
        },
        NeedsHuman::WorktreeUnreadable {
            path: path("app/.git/worktrees/x"),
        },
        NeedsHuman::UnlistedGitDir {
            git_dir: "/home/me/hand/.git".into(),
            head: UnprobedHead::Branch {
                name: "other".into(),
            },
            busy: vec![Session::at(
                1400,
                0,
                "/home/me/hand".into(),
                SessionSource::SessionFile,
            )],
        },
    ];
    e.unprobed_worktrees = unprobed_worktrees();
    e
}

fn git_dir(id: &str) -> String {
    path(&format!("app/.git/worktrees/{id}"))
}

fn unprobed(dir: &str, head: UnprobedHead, why: UnprobedWhy) -> UnprobedWorktree {
    UnprobedWorktree {
        path: path(dir),
        git_dir: Some(git_dir(dir)),
        head,
        locked: false,
        in_progress: None,
        why,
        holds: None,
    }
}

fn unprobed_on(name: &str) -> UnprobedHead {
    UnprobedHead::Branch { name: name.into() }
}

/// Every `UnprobedWhy`, every `UnprobedHead`, every `Prune`, and every
/// `PruneLoss` — and one a live session works in.
fn unprobed_worktrees() -> Vec<UnprobedWorktreeStatus> {
    let nothing_held = GitDirHolds {
        submodules: false,
        worktree_refs: false,
        staged: Some(false),
    };
    vec![
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(nothing_held),
                ..unprobed("app-gone", unprobed_on("gone"), UnprobedWhy::Prunable)
            },
            prune: Some(Prune::Safe),
            // a session still in its deleted dir
            busy: vec![Session {
                worktree: Some(path("app-gone")),
                ..Session::at(4343, 0, path("app-gone/src"), SessionSource::RosterWorker)
            }],
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                in_progress: Some(InProgressOp::Revert),
                holds: Some(GitDirHolds {
                    submodules: true,
                    worktree_refs: true,
                    staged: None,
                }),
                ..unprobed(
                    "app-spike",
                    UnprobedHead::Detached {
                        commit: "89abcdef0123456789abcdef0123456789abcdef".into(),
                    },
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: prune_losses(),
            }),
            busy: vec![],
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(nothing_held),
                ..unprobed("app-b", unprobed_on("b"), UnprobedWhy::Prunable)
            },
            prune: Some(Prune::Moved {
                to: vec!["b-copy".into(), "b-moved".into()],
            }),
            busy: vec![],
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                path: "/media/usb/app".into(),
                locked: true,
                in_progress: Some(InProgressOp::Bisect),
                ..unprobed("usb", unprobed_on("usb"), UnprobedWhy::Missing)
            },
            prune: None,
            busy: vec![],
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                path: git_dir("x"),
                git_dir: None,
                ..unprobed(
                    "x",
                    UnprobedHead::Unknown,
                    UnprobedWhy::Failed {
                        error: "not listed by git: reading HEAD: Permission denied".into(),
                    },
                )
            },
            prune: None,
            busy: vec![],
        },
    ]
}

fn prune_losses() -> Vec<PruneLoss> {
    vec![
        PruneLoss::Operation {
            op: InProgressOp::Revert,
        },
        PruneLoss::DetachedHead,
        PruneLoss::UnknownHead,
        PruneLoss::MissingBranch {
            name: "spike".into(),
        },
        PruneLoss::Submodules,
        PruneLoss::WorktreeRefs,
        PruneLoss::StagedChanges,
        PruneLoss::UnmatchedGitDir,
        PruneLoss::RelativeGitdir {
            git_dir: git_dir("k"),
        },
    ]
}

/// Origin drift, holding the entry's push; a merge in its primary; a
/// worktree whose path can't be resolved; the fetch failed.
fn blog() -> EntryStatus {
    let mut e = entry("fuz_blog", Some("main"));
    e.url = "https://github.com/fuzdev/fuz_blog".into();
    e.checkouts[0].in_progress = Some(InProgressOp::Merge);
    e.branches = vec![BranchStatus {
        unique_commits: 2,
        worktree: Some(path("fuz_blog")),
        ..branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 2 },
            Verdict::Held {
                action: SyncAction::Push { commits: 2 },
                by: HeldBy::Entry,
            },
        )
    }];
    e.needs_human = vec![
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:ryanatkn/fuz_blog".into(),
            },
            expected: "git@github.com:fuzdev/fuz_blog".into(),
            fix: OriginFix::SetUrl,
        },
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_blog"),
            op: InProgressOp::Merge,
        },
        NeedsHuman::CheckoutUnresolvable {
            checkout: "/home/me/sealed/fuz_blog-wt".into(),
            path: "/home/me/sealed/fuz_blog-wt".into(),
            error: "Permission denied (os error 13)".into(),
        },
    ];
    e.fetch_error = Some(RemoteFailure::RefGone {
        refname: "refs/heads/dev".into(),
        fix: RefGoneFix::SetBranches {
            branch: Some("main".into()),
        },
    });
    e
}

/// Archived and private, no CI, with a commit ahead its host refuses, and
/// no `origin` — and anyone can read it.
fn archived() -> EntryStatus {
    let mut e = entry("old", Some("main"));
    e.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Missing,
        expected: "git@github.com:me/old".into(),
        fix: OriginFix::Add,
    }];
    e.archived = true;
    e.visibility = Some(Visibility::Private);
    e.visibility_check = Some(VisibilityCheck::Leak);
    e.ci = false;
    e.fetched_at = Some(NOW - 300 * DAY);
    e.branches = vec![BranchStatus {
        unique_commits: 1,
        worktree: Some(path("old")),
        ..branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 1 },
            Verdict::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead,
            },
        )
    }];
    e
}

/// A third-party reference left at its HEAD: shallow and sparse, detached
/// mid-rebase, one branch to move and one with local work off the tip.
fn test262() -> EntryStatus {
    let mut e = entry("test262", None);
    e.kind = EntryKind::Reference;
    e.url = "https://github.com/tc39/test262".into();
    e.writable = false;
    e.visibility = None;
    e.ci = false;
    e.layout = Some(Layout {
        shallow: true,
        sparse: true,
        partial_filter: None,
    });
    e.checkouts[0].head = Head::Detached {
        commit: "fedcba9876543210fedcba9876543210fedcba98".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Rebase);
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Shallow,
            Verdict::Act {
                action: SyncAction::Move,
            },
        ),
        BranchStatus {
            unique_commits: 2,
            ..branch(
                "work",
                Some("origin/work"),
                Relation::Shallow,
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::ShallowLocalWork,
                },
            )
        },
        BranchStatus {
            unique_commits: 4,
            ..branch("audit", None, Relation::Untracked, Verdict::LocalOnly)
        },
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("test262"),
            op: InProgressOp::Rebase,
        },
        // its URL list reset by an empty value
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "https://github.com/tc39/test262".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::EmptyValue,
            },
        },
    ];
    e.fetched_at = None;
    e
}

/// An owned fork kept as a reference, pinned on the branch it lives on and
/// behind a stale remote-tracking ref (the pin holds the fast-forward), with
/// a `git am` stopped mid-way and a valueless `origin` URL.
fn spec() -> EntryStatus {
    let mut e = entry("ecma262", Some("draft"));
    e.pinned = true;
    e.kind = EntryKind::Reference;
    e.visibility = None;
    e.checkouts[0].head = on("draft");
    e.checkouts[0].in_progress = Some(InProgressOp::Am);
    e.branches = vec![branch(
        "draft",
        Some("origin/draft"),
        Relation::Behind { commits: 5 },
        Verdict::Held {
            action: SyncAction::FastForward { commits: 5 },
            by: HeldBy::Pinned,
        },
    )];
    e.needs_human = vec![
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/ecma262".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::ValuelessUrl,
            },
        },
        NeedsHuman::OperationInProgress {
            checkout: path("ecma262"),
            op: InProgressOp::Am,
        },
    ];
    e
}

/// Following `dev`, which is missing, detached where it shouldn't be, with a
/// sequencer stopped in it; `main` has no upstream.
fn zzz() -> EntryStatus {
    let mut e = entry("zzz", Some("dev"));
    e.fetch_error = Some(RemoteFailure::Unreachable {
        cause: UnreachableCause::Connection,
        message: "ssh: connect to host github.com port 22: Connection refused".into(),
    });
    e.checkouts[0].head = Head::Detached {
        commit: "00112233445566778899aabbccddeeff00112233".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Sequencer);
    e.branches = vec![branch("main", None, Relation::Untracked, Verdict::Quiet)];
    e.needs_human = vec![
        NeedsHuman::DefaultBranchMissing {
            branch: "dev".into(),
        },
        NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into(),
        },
        NeedsHuman::UnexpectedDetached {
            checkout: path("zzz"),
        },
        NeedsHuman::OperationInProgress {
            checkout: path("zzz"),
            op: InProgressOp::Sequencer,
        },
    ];
    e
}

/// Not cloned yet: sync would clone it, over SSH on its branch.
fn missing() -> EntryStatus {
    EntryStatus {
        presence: Presence::Missing,
        clone: Some(CloneVerdict::Act {
            recipe: CloneRecipe {
                url: "git@github.com:me/blake3".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
        }),
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        ..entry("blake3", Some("main"))
    }
}

/// A missing third-party reference, `key`, cloned over HTTPS from the
/// remote's default branch, shallow and sparse — its clone `held` or not.
fn missing_reference(key: &str, held: Option<HeldBy>) -> EntryStatus {
    let recipe = CloneRecipe {
        url: format!("https://github.com/them/{key}"),
        branch: None,
        shallow: true,
        sparse: Some("css".into()),
    };
    EntryStatus {
        kind: EntryKind::Reference,
        url: format!("https://github.com/them/{key}"),
        writable: false,
        visibility: None,
        ci: false,
        branch: None,
        presence: Presence::Missing,
        clone: Some(match held {
            Some(by) => CloneVerdict::Held { recipe, by },
            None => CloneVerdict::Act { recipe },
        }),
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        ..entry(key, None)
    }
}

/// A missing entry naming app's repo, held for a person: its dir may have
/// been a worktree of app's.
fn twin() -> EntryStatus {
    EntryStatus {
        url: "https://github.com/me/app".into(),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/app".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: HeldBy::Entry,
        }),
        needs_human: vec![NeedsHuman::CloneSharesRepo { with: "app".into() }],
        ..EntryStatus {
            key: "twin".into(),
            dir: "twin".into(),
            ..missing()
        }
    }
}

/// A missing entry whose repo the unregistered dir `renamed-old` clones,
/// held for a person: likely its checkout under another name.
fn renamed() -> EntryStatus {
    EntryStatus {
        url: "https://github.com/me/renamed".into(),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/renamed".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: HeldBy::Entry,
        }),
        needs_human: vec![NeedsHuman::ClonedUnregistered {
            dir: "renamed-old".into(),
        }],
        ..EntryStatus {
            key: "renamed".into(),
            dir: "renamed".into(),
            ..missing()
        }
    }
}

/// The unregistered dir `renamed()` is held by: a clone of its repo.
fn renamed_old() -> UnregisteredClone {
    stray(
        "renamed-old",
        Some("git@github.com:me/renamed"),
        true,
        UnregisteredKind::Clone,
    )
}

/// A dir that holds no repo.
fn not_a_repo() -> EntryStatus {
    EntryStatus {
        presence: Presence::NotARepo,
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        needs_human: vec![NeedsHuman::NotARepo {
            detail: "empty directory".into(),
        }],
        ..entry("goblins", Some("main"))
    }
}

/// A partial clone whose probe failed on a missing object: the layout read
/// first stands, the rest is incomplete.
fn partial() -> EntryStatus {
    EntryStatus {
        layout: Some(Layout {
            partial_filter: Some("tree:0".into()),
            ..plain_layout()
        }),
        checkouts: vec![],
        fetched_at: None,
        probe_error: Some("git status failed (128): error: bad tree object HEAD".into()),
        fetch_error: Some(RemoteFailure::RepoNotFound {
            message: "ERROR: Repository not found.".into(),
        }),
        ..entry("wpt", Some("main"))
    }
}

/// Private as declared; its key refused.
/// A fetch refused by the host: its branch behind stays put
/// (`HeldBy::FetchFailed`).
fn forge() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Auth,
            message: "git@github.com: Permission denied (publickey).".into(),
        }),
        visibility_check: Some(VisibilityCheck::Private),
        branches: vec![BranchStatus {
            worktree: Some(path("fuz_forge")),
            ..branch(
                "main",
                Some("origin/main"),
                Relation::Behind { commits: 2 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 2 },
                    by: HeldBy::FetchFailed,
                },
            )
        }],
        ..entry("fuz_forge", Some("main"))
    }
}

/// A branch on HEAD in two clean checkouts (`worktree add -f`): its
/// fast-forward held (`HeldBy::SeveralCheckouts`).
fn gro() -> EntryStatus {
    let mut e = entry("gro", Some("main"));
    e.checkouts.push(Checkout {
        path: path("gro-twin"),
        primary: false,
        head: on("main"),
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: None,
        busy: vec![],
    });
    e.branches = vec![BranchStatus {
        worktree: Some(path("gro")),
        ..branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 3 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 3 },
                by: HeldBy::SeveralCheckouts,
            },
        )
    }];
    e
}

/// Private, with the network down: its host not found, the check timed out.
fn zap() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Dns,
            message: "ssh: Could not resolve hostname github.com: Temporary failure in name \
                      resolution"
                .into(),
        }),
        visibility_check: Some(VisibilityCheck::Unknown {
            failure: RemoteFailure::TimedOut { after_secs: 120 },
        }),
        ..entry("zap", Some("main"))
    }
}

/// A branch deleted on the remote, named by one of several fetch refspecs;
/// an old `origin` URL beside a mirror.
fn mdz() -> EntryStatus {
    EntryStatus {
        needs_human: vec![NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:old/mdz".into(),
            },
            expected: "git@github.com:me/mdz".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::SeveralUrls,
            },
        }],
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/attrs".into(),
            fix: RefGoneFix::UnsetRefspec {
                pattern: r"^\+?refs/heads/attrs(:|$)".into(),
            },
        }),
        ..entry("mdz", Some("main"))
    }
}

/// A single-branch clone whose own branch, the registry's, is gone: no
/// branch to name.
fn tsv() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/main".into(),
            fix: RefGoneFix::SetBranches { branch: None },
        }),
        ..entry("tsv", Some("main"))
    }
}

/// A remote named `origin/fork`, its refs under origin's: `status --fetch`
/// doesn't fetch.
fn fuz_css() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::OriginRefsShared {
            remote: "origin/fork".into(),
            refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
        }),
        ..entry("fuz_css", Some("main"))
    }
}

/// A legacy remotes file that can't be read: `status --fetch` doesn't
/// fetch.
fn fuz_ui() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::LegacyRemotesUnreadable {
            path: path("fuz_ui/.git/remotes/old"),
        }),
        ..entry("fuz_ui", Some("main"))
    }
}

/// A refspec writing tags: `status --fetch` doesn't fetch it.
fn tsv_fuz_dev() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefspecOutsideOrigin {
            refspec: "+refs/tags/*:refs/tags/*".into(),
        }),
        ..entry("tsv.fuz.dev", Some("main"))
    }
}

/// A missing ref no refspec in the repo's config names.
fn uz() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "typecheck-arc".into(),
            fix: RefGoneFix::ByHand,
        }),
        ..entry("uz", Some("main"))
    }
}

/// Pushes that can't go through origin: a push URL rewritten to another
/// repo, holding the one ahead (`PushUrl`), and a branch ahead of
/// `origin/HEAD`, whose ref on origin isn't a branch.
fn pushy() -> EntryStatus {
    let push = SyncAction::Push { commits: 1 };
    EntryStatus {
        branches: vec![
            BranchStatus {
                unique_commits: 1,
                ..branch(
                    "main",
                    Some("origin/main"),
                    Relation::Ahead { commits: 1 },
                    Verdict::Held {
                        action: push,
                        by: HeldBy::PushUrl,
                    },
                )
            },
            BranchStatus {
                unique_commits: 1,
                ..branch(
                    "tip",
                    Some("origin/HEAD"),
                    Relation::Ahead { commits: 1 },
                    Verdict::NeedsHuman {
                        reason: BranchNeedsHuman::UpstreamNotABranch,
                    },
                )
            },
        ],
        needs_human: vec![NeedsHuman::PushUrlMismatch {
            push_urls: vec![
                "git@github.com:me/pushy".into(),
                "https://***@mirror.example.com/me/pushy".into(),
            ],
            expected: "git@github.com:me/pushy".into(),
        }],
        ..entry("pushy", Some("main"))
    }
}

/// An agent's run: a push nothing else holds waits for the gateway.
fn agent_run() -> EntryStatus {
    EntryStatus {
        branches: vec![BranchStatus {
            unique_commits: 2,
            ..branch(
                "main",
                Some("origin/main"),
                Relation::Ahead { commits: 2 },
                Verdict::Held {
                    action: SyncAction::Push { commits: 2 },
                    by: HeldBy::Gateway,
                },
            )
        }],
        ..entry("agent_run", Some("main"))
    }
}

/// A host key ssh doesn't trust; an old origin, with a token in it
/// (redacted), set in global config.
fn site() -> EntryStatus {
    EntryStatus {
        needs_human: vec![NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "https://***@github.com/old/site".into(),
            },
            expected: "git@github.com:me/site".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile,
            },
        }],
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::HostKey,
            message: "Host key verification failed.".into(),
        }),
        ..entry("site", Some("main"))
    }
}

// --- unregistered ---

fn stray(
    dir: &str,
    origin: Option<&str>,
    owned: bool,
    kind: UnregisteredKind,
) -> UnregisteredClone {
    UnregisteredClone {
        dir: dir.into(),
        origin: origin.map(str::to_owned),
        owned,
        kind,
    }
}

fn moved(blocked_by: Option<RepairBlock>, exit_noise: Option<&str>) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by,
        exit_noise: exit_noise.map(str::to_owned),
    }
}

/// Every `UnregisteredKind` and `RepairBlock`, over each ownership.
fn unregistered() -> Vec<UnregisteredClone> {
    let app_origin = Some("git@github.com:me/app");
    vec![
        stray(
            ".blake3.repos-clone-4242-0123456789abcdef",
            Some("git@github.com:me/blake3"),
            true,
            UnregisteredKind::UnfinishedClone,
        ),
        stray(
            "b-copy",
            app_origin,
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: Some(path("b-moved")),
            },
        ),
        stray("b-moved", app_origin, true, moved(None, Some("/home/me/y"))),
        stray(
            "c-copy",
            app_origin,
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: None,
            },
        ),
        stray(
            "claimed",
            app_origin,
            true,
            moved(
                Some(RepairBlock::ClaimedDir {
                    git_dir: git_dir("claimed"),
                }),
                None,
            ),
        ),
        stray(
            "lib",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Clone,
        ),
        stray(
            "lib-feat",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Worktree,
        ),
        stray(
            "mine",
            Some("git@github.com:me/mine"),
            true,
            UnregisteredKind::Clone,
        ),
        stray(
            "rel",
            app_origin,
            true,
            moved(
                Some(RepairBlock::RelativeGitdir {
                    git_dir: git_dir("k"),
                }),
                None,
            ),
        ),
        renamed_old(),
        stray(
            "rewrites",
            app_origin,
            true,
            moved(
                Some(RepairBlock::Rewrites {
                    path: path("q"),
                    git_dir: git_dir("q"),
                }),
                None,
            ),
        ),
        stray(
            "scratch",
            None,
            false,
            UnregisteredKind::OrphanedWorktree {
                entry: "app".into(),
            },
        ),
        stray(
            "unreadable",
            app_origin,
            true,
            moved(
                Some(RepairBlock::UnreadableGitdir {
                    git_dir: git_dir("u"),
                }),
                None,
            ),
        ),
        stray(
            "v\u{fffd}",
            app_origin,
            true,
            moved(Some(RepairBlock::NonUtf8Path), None),
        ),
        stray(
            "w-nul",
            app_origin,
            true,
            moved(
                Some(RepairBlock::NulInGitdir {
                    git_dir: git_dir("w-nul"),
                }),
                None,
            ),
        ),
        stray(
            "wa",
            app_origin,
            true,
            moved(
                Some(RepairBlock::Swapped {
                    git_dir: git_dir("wa"),
                    with: "wb".into(),
                }),
                None,
            ),
        ),
    ]
}

// --- the error document ---

fn repo_name(key: &str) -> EntryName {
    EntryName {
        kind: EntryKind::Repo,
        key: key.into(),
    }
}

/// Every `RegistryIssue`.
fn registry_issues() -> Vec<RegistryIssue> {
    vec![
        RegistryIssue::RepoNotOwned {
            key: "kit".into(),
            account: "sveltejs".into(),
        },
        RegistryIssue::ForkNotOwned { key: "wpt".into() },
        RegistryIssue::DirNotAName {
            entry: repo_name("app"),
            dir: "../app".into(),
        },
        RegistryIssue::DirClaimedTwice {
            dir: "app".into(),
            first: repo_name("app"),
            second: EntryName {
                kind: EntryKind::Reference,
                key: "app-ref".into(),
            },
        },
        RegistryIssue::KeyInBoth { key: "gro".into() },
        RegistryIssue::KeyIsOtherDir {
            key: "site".into(),
            entry: repo_name("www"),
        },
        RegistryIssue::UnknownCheckoutRef {
            key: "app".into(),
            field: CheckoutList::Requires,
            target: "nope".into(),
        },
        RegistryIssue::SelfRef {
            key: "app".into(),
            field: CheckoutList::Consults,
        },
        RegistryIssue::RequiresAndConsults {
            key: "app".into(),
            target: "gro".into(),
        },
    ]
}
