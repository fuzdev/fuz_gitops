//! The `--json` contract's golden fixtures: hand-built documents, serialized
//! and compared with the JSON checked in at the repo root's
//! `src/test/fixtures/repos_status/`, where the TS side reads them.
//!
//! Compared as parsed JSON, so a formatter's reflow of a checked-in file
//! can't fail the test. Never hand-edit the files: regenerate them with
//! `UPDATE_GOLDEN=1 cargo test --test golden`, and bump
//! `STATUS_FORMAT_VERSION` (and with it `SYNC_FORMAT_VERSION` and
//! `PUSH_FORMAT_VERSION`, whose documents embed the status report) when the
//! change breaks the shape; a change to the sync or push document alone
//! bumps its own version.
//!
//! Every report `docs` builds is checked for its structure (`invariants`): no
//! shape a report can't have, whatever `classify` decides — the integration
//! tests over real git pin its decisions. Between them the status documents
//! — the whole workspace's report, the targeted one, `sessions.json` (every
//! state of busy detection, which a report carries one of), and an
//! `error_report_<kind>.json` per error `repos status --json` can print —
//! cover every variant of every closed enum the status report and its error
//! document carry, in every place each can appear; the sync and push
//! documents cover every outcome of theirs (`coverage`). Each enum's
//! variants are listed once, a list an exhaustive `match` checks, and the
//! floor counts that list: a new variant fails to compile until it's
//! listed, and fails the floor until a golden covers it.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

#[path = "golden/coverage.rs"]
mod coverage;
#[path = "golden/docs.rs"]
mod docs;
#[path = "golden/invariants.rs"]
mod invariants;

use std::path::{Path, PathBuf};

use fuz_repos::busy::Sessions;
use fuz_repos::classify::NeedsHuman;
use fuz_repos::error::Error;
use fuz_repos::remote::RemoteFailure;
use fuz_repos::report::{EntryStatus, ErrorReport, FetchOutcome, PushOutcome};
use fuz_repos::state::{
    AtRest, BranchNeedsHuman, BranchStatus, Presence, RefreshVerdict, Relation, SyncAction, Verdict,
};
use fuz_repos::{PUSH_FORMAT_VERSION, STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use serde::Serialize;
use serde_json::Value;

use docs::{
    branch, checked_out, entry, missing, on, push_report_doc, report, sessions_doc, status_errors,
    status_report_doc, sync_report_doc, targeted_doc, third_party,
};

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
             regenerate with `UPDATE_GOLDEN=1 cargo test --test golden`, and bump STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION, or PUSH_FORMAT_VERSION if it breaks the shape",
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
    // at-rest facts exactly for an entry with its primary read, each fact
    // both ways, and each with nothing to say
    assert!(
        doc.entries
            .iter()
            .all(|e| e.at_rest.is_some() != e.checkouts.is_empty())
    );
    assert!(doc.entries.iter().any(|e| e.at_rest.is_none()));
    let facts: Vec<AtRest> = doc.entries.iter().filter_map(|e| e.at_rest).collect();
    for on_branch in [Some(true), Some(false), None] {
        assert!(
            facts.iter().any(|r| r.on_branch == on_branch),
            "{on_branch:?}"
        );
    }
    for fact in [
        |r: &AtRest| r.clean,
        |r: &AtRest| r.idle,
        |r: &AtRest| r.followed.is_some(),
    ] {
        assert!(facts.iter().any(fact) && !facts.iter().all(fact));
    }
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
    assert_golden("status_report_targeted.json", &doc);
}

/// The structural checks refuse a report no run could print, and the floor
/// a document set short of a variant.
#[test]
fn malformed_reports_and_uncovered_variants_fail() {
    // refused, for the reason `expect` names
    let refused = |expect: &str, entries: Vec<EntryStatus>| {
        let err = std::panic::catch_unwind(|| {
            report(
                true,
                Sessions::Available { unscoped: vec![] },
                entries,
                None,
            )
        })
        .expect_err(expect);
        let message = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains(expect), "{expect}: refused for {message}");
    };
    let mut e = entry("app", Some("main"));
    e.fetch_error = Some(RemoteFailure::RepoNotFound {
        message: "ERROR: Repository not found.".into(),
    });
    refused(
        "dated by the `FETCH_HEAD` its failed fetch emptied",
        vec![e],
    );
    // an unasked reference, compared against origin
    let mut e = third_party("lib", None);
    e.branches = vec![BranchStatus {
        unique_commits: 1,
        ..branch(
            "work",
            Some("origin/work"),
            Relation::Shallow,
            Verdict::NeedsHuman {
                reason: BranchNeedsHuman::ShallowLocalWork,
            },
        )
    }];
    refused("an untracked entry lists only local work", vec![e]);
    refused(
        "entry key `app` twice",
        vec![entry("app", Some("main")), entry("app", Some("main"))],
    );
    // classify's if/else-if chain says one at most
    let mut e = entry("zzz", Some("dev"));
    e.branches = vec![checked_out(
        branch("main", None, Relation::Untracked, Verdict::Quiet),
        "zzz",
    )];
    e.checkouts[0].head = on("main");
    e.needs_human = vec![
        NeedsHuman::DefaultBranchMissing {
            branch: "dev".into(),
        },
        NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into(),
        },
    ];
    refused("several default-branch reasons", vec![e]);
    let mut e = entry("app", Some("main"));
    e.branches[0].relation = Relation::Shallow;
    e.branches[0].verdict = Verdict::Act {
        action: SyncAction::Move,
    };
    refused("shallow in a full clone", vec![e]);
    let mut e = missing();
    e.stashes = 1;
    refused("facts read of no repo", vec![e]);
    // the targeted document alone lacks most variants
    let floor = std::panic::catch_unwind(|| {
        coverage::assert_status_coverage(&[&targeted_doc()], &sessions_doc(), &[]);
    })
    .expect_err("floor");
    let message = floor.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("no golden covers"), "floor: {message}");
}

/// The status documents together carry every variant of the report's and
/// the error document's enums, each in every place it can appear.
#[test]
fn status_coverage() {
    let errors: Vec<ErrorReport> = status_errors()
        .iter()
        .map(|e| ErrorReport::new(e, STATUS_FORMAT_VERSION))
        .collect();
    coverage::assert_status_coverage(
        &[&status_report_doc(), &targeted_doc()],
        &sessions_doc(),
        &errors.iter().collect::<Vec<_>>(),
    );
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
    coverage::assert_sync_coverage(&doc);
    assert_golden("sync_report.json", &doc);
}

/// Every fatal error `repos status --json` can print, one document each,
/// named for its kind: `error_report_<kind>.json`. `missing_command` never
/// has a document (with no subcommand, no `--json` is known), and the push
/// errors are `repos push`'s alone.
#[test]
fn error_reports() {
    let mut names = std::collections::BTreeSet::new();
    for e in status_errors() {
        let doc = ErrorReport::new(&e, STATUS_FORMAT_VERSION);
        assert_eq!(doc.version, STATUS_FORMAT_VERSION);
        let kind = serde_json::to_value(&doc.error.kind).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned();
        let name = format!("error_report_{kind}.json");
        assert!(names.insert(name.clone()), "{kind} twice");
        assert_golden(&name, &doc);
    }
    // no stale document left beside them
    let on_disk: std::collections::BTreeSet<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("error_report"))
        .collect();
    assert_eq!(on_disk, names);
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
fn push_report() {
    let doc = push_report_doc();
    assert_eq!(doc.version, PUSH_FORMAT_VERSION);
    assert_eq!(doc.status.version, STATUS_FORMAT_VERSION);
    assert!(doc.status.fetched);
    assert!(doc.status.unregistered.is_none());
    // each target names an entry of the status, and a branch it has —
    // but a branch with no commit yet, which fails
    for p in &doc.pushes {
        let e = doc
            .status
            .entries
            .iter()
            .find(|e| e.key == p.key)
            .unwrap_or_else(|| panic!("no status for {}", p.key));
        if let (Some(name), false) = (&p.branch, matches!(p.outcome, PushOutcome::Failed { .. })) {
            assert!(
                e.branches.iter().any(|b| b.name == *name),
                "{}:{name}",
                p.key
            );
        }
    }
    // something didn't push: exit 1
    assert!(!doc.in_sync());
    coverage::assert_push_coverage(&doc);
    assert_golden("push_report.json", &doc);
}

/// A fatal error only `repos push` makes, at its report's version.
#[test]
fn push_error_report() {
    let doc = ErrorReport::new(
        &Error::PushThirdParty {
            key: "typescript".into(),
        },
        PUSH_FORMAT_VERSION,
    );
    assert_eq!(doc.version, PUSH_FORMAT_VERSION);
    assert_golden("push_error_report.json", &doc);
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
