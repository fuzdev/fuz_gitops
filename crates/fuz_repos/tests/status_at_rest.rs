//! Each entry's at-rest facts from real repos: whether its primary checkout
//! is on the branch it follows, clean, and idle, and that branch's relation
//! — claimed only where one was computed — plus the text summary's counts,
//! which read them.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::process::Output;

use fuz_repos::classify::Refresh;
use fuz_repos::state::{AtRest, InProgressOp, Presence, Relation};
use serde_json::{Value, json};
use support::{FixtureWorkspace, OWNER, THIRD_PARTY, branch, find_entry};

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

fn repos(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> Output {
    ws.command(REPOS, cwd).args(args).output().unwrap()
}

fn stdout(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

const fn rest(on_branch: Option<bool>, followed: Option<Relation>) -> AtRest {
    AtRest {
        on_branch,
        clean: true,
        idle: true,
        followed,
    }
}

/// Owned repos off their followed branch — on another one, or following a
/// branch that doesn't exist locally — dirty, mid-merge, behind, and at
/// rest.
#[test]
fn an_owned_entrys_checkout() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.assert_clean(&app);
    ws.assert_head(&app, Some("main"));
    ws.assert_track(&app, "main", "");

    // on `feat`, which tracks origin's, in sync
    ws.remote("feat", &[]);
    ws.upstream_commit("feat", "feat");
    ws.declare_repo("feat", "feat", "");
    let feat = ws.clone_owned("feat", "feat", &[]);
    ws.git(&feat, &["checkout", "-q", "feat"]);
    ws.assert_head(&feat, Some("feat"));
    ws.assert_upstream(&feat, "feat", "refs/remotes/origin/feat");
    ws.assert_track(&feat, "feat", "");
    ws.assert_track(&feat, "main", "");
    ws.assert_clean(&feat);

    let dirty = ws.owned_repo("dirty", &[]);
    support::write(&dirty, "scratch.txt", "untracked\n");
    ws.assert_porcelain(&dirty, &["?? scratch.txt"]);

    // a merge stopped on a conflict, a local commit ahead of origin
    let mid = ws.owned_repo("mid", &[("x.txt", "base\n")]);
    ws.git(&mid, &["checkout", "-q", "-b", "other"]);
    support::write(&mid, "x.txt", "theirs\n");
    ws.git(&mid, &["commit", "-q", "-am", "theirs"]);
    ws.git(&mid, &["checkout", "-q", "main"]);
    support::write(&mid, "x.txt", "ours\n");
    ws.git(&mid, &["commit", "-q", "-am", "ours"]);
    ws.git_fails(&mid, &["merge", "-q", "other"]);
    assert!(mid.join(".git/MERGE_HEAD").is_file());
    ws.assert_porcelain(&mid, &["UU x.txt"]);
    ws.assert_head(&mid, Some("main"));
    ws.assert_track(&mid, "main", "[ahead 1]");

    // behind: the fetch saw an upstream commit
    let behind = ws.owned_repo("behind", &[]);
    ws.upstream_commit("behind", "main");
    ws.git(&behind, &["fetch", "-q"]);
    ws.assert_track(&behind, "main", "[behind 1]");
    ws.assert_clean(&behind);

    // following `dev`, which it has no local branch of
    ws.remote("nodev", &[]);
    ws.declare_repo("nodev", "nodev", "branch = \"dev\"");
    let nodev = ws.clone_owned("nodev", "nodev", &[]);
    assert!(!ws.has_ref(&nodev, "refs/heads/dev"));
    ws.assert_head(&nodev, Some("main"));

    let entries = ws.status();
    let at_rest = |key: &str| find_entry(&entries, key).at_rest.unwrap();
    assert_eq!(at_rest("app"), rest(Some(true), Some(Relation::InSync)));
    assert_eq!(at_rest("feat"), rest(Some(false), Some(Relation::InSync)));
    assert_eq!(
        at_rest("dirty"),
        AtRest {
            clean: false,
            ..rest(Some(true), Some(Relation::InSync))
        }
    );
    let e = find_entry(&entries, "mid");
    assert_eq!(e.checkouts[0].in_progress, Some(InProgressOp::Merge));
    assert_eq!(
        e.at_rest,
        Some(AtRest {
            on_branch: Some(true),
            clean: false,
            idle: false,
            followed: Some(Relation::Ahead { commits: 1 }),
        })
    );
    assert_eq!(
        at_rest("behind"),
        rest(Some(true), Some(Relation::Behind { commits: 1 }))
    );
    // the followed branch's relation is its entry in `branches`
    let e = find_entry(&entries, "behind");
    assert_eq!(
        e.at_rest.unwrap().followed,
        Some(branch(e, "main").relation)
    );
    assert_eq!(at_rest("nodev"), rest(Some(false), None));
}

/// A third-party reference no run asks about is compared against no
/// remote: its branch with local work reads `Untracked`, a relation never
/// computed, so nothing is claimed of the branch it follows; named, it's
/// refreshed, and the relation is claimed.
#[test]
fn a_references_followed_branch_is_claimed_only_when_compared() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "branch = \"main\"");
    let lib = ws.clone_third_party_over_https("lib", "lib", &[]);
    ws.commit(&lib, "local");
    ws.assert_head(&lib, Some("main"));
    ws.assert_track(&lib, "main", "[ahead 1]");
    ws.assert_clean(&lib);

    let unasked = ws.status();
    let e = find_entry(&unasked, "lib");
    assert_eq!(e.refresh, None);
    assert_eq!(branch(e, "main").relation, Relation::Untracked);
    assert_eq!(e.at_rest, Some(rest(Some(true), None)));

    let named = ws.status_asked(false, Refresh::Named);
    let e = find_entry(&named, "lib");
    assert_eq!(branch(e, "main").relation, Relation::Ahead { commits: 1 });
    assert_eq!(
        e.at_rest,
        Some(rest(Some(true), Some(Relation::Ahead { commits: 1 })))
    );
}

/// A pin's facts are decided as any entry's: owned, its branches compared
/// from local refs; third-party, never. A reference following no branch
/// says nothing of HEAD's place, whatever it is.
#[test]
fn pins_and_references_with_no_branch() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("fork", &[]);
    ws.declare_reference("fork", OWNER, "fork", "pinned = true\nbranch = \"main\"");
    let fork = ws.clone_owned("fork", "fork", &[]);
    ws.assert_head(&fork, Some("main"));
    ws.assert_track(&fork, "main", "");

    ws.remote("oracle", &[]);
    ws.declare_reference(
        "oracle",
        THIRD_PARTY,
        "oracle",
        "pinned = true\nbranch = \"main\"",
    );
    let oracle = ws.clone_third_party("oracle", "oracle", &[]);
    ws.git(&oracle, &["checkout", "-q", "--detach"]);
    ws.assert_head(&oracle, None);

    ws.remote("spec", &[]);
    ws.declare_reference("spec", THIRD_PARTY, "spec", "");
    let spec = ws.clone_third_party("spec", "spec", &[]);
    ws.git(&spec, &["checkout", "-q", "--detach"]);
    ws.assert_head(&spec, None);
    ws.assert_clean(&spec);

    let entries = ws.status();
    let fork = find_entry(&entries, "fork");
    assert!(fork.pinned && fork.writable);
    assert_eq!(fork.at_rest, Some(rest(Some(true), Some(Relation::InSync))));
    let oracle = find_entry(&entries, "oracle");
    assert!(oracle.pinned && !oracle.writable);
    assert_eq!(oracle.at_rest, Some(rest(Some(false), None)));
    let spec = find_entry(&entries, "spec");
    assert!(!spec.pinned && spec.branch.is_none());
    assert_eq!(spec.at_rest, Some(rest(None, None)));
}

/// No primary checkout read, no facts: a missing entry, a dir that holds
/// no repo, and a repo whose probe failed — a bare one, with no checkout to
/// read.
#[test]
fn no_facts_without_a_checkout() {
    let mut ws = FixtureWorkspace::new();
    ws.declare_repo("gone", "gone", "");
    ws.declare_repo("empty", "empty", "");
    std::fs::create_dir(ws.dir("empty")).unwrap();
    assert!(!ws.dir("gone").exists());
    ws.remote("bare", &[]);
    ws.declare_repo("bare", "bare", "");
    let bare = ws.dir("bare");
    ws.git(
        &ws.root(),
        &[
            "clone",
            "-q",
            "--bare",
            ws.bare("bare").to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    assert_eq!(
        ws.git(&bare, &["rev-parse", "--is-bare-repository"]),
        "true"
    );

    let entries = ws.status();
    for (key, presence) in [
        ("gone", Presence::Missing),
        ("empty", Presence::NotARepo),
        ("bare", Presence::Present),
    ] {
        let e = find_entry(&entries, key);
        assert_eq!(e.presence, presence, "{key}");
        assert!(e.checkouts.is_empty(), "{key}");
        assert_eq!(e.at_rest, None, "{key}");
    }
    assert!(find_entry(&entries, "bare").probe_error.is_some());
}

/// The followed branch held in a linked worktree while the primary is on
/// another: off it, and the branch's relation read from local refs all the
/// same.
#[test]
fn the_followed_branch_in_a_linked_worktree() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["checkout", "-q", "feat"]);
    let wt = ws.outside("app-main");
    ws.add_worktree(&app, &wt, &["main"]);
    ws.assert_head(&app, Some("feat"));
    ws.assert_head(&wt, Some("main"));
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q"]);
    ws.assert_track(&app, "main", "[behind 1]");
    ws.assert_track(&app, "feat", "");
    ws.assert_clean(&app);
    ws.assert_clean(&wt);

    let e = ws.entry("app");
    assert_eq!(e.checkouts.len(), 2);
    assert!(e.checkouts[0].primary && !e.checkouts[1].primary);
    assert_eq!(branch(&e, "main").worktree.as_deref(), wt.to_str());
    assert_eq!(
        e.at_rest,
        Some(rest(Some(false), Some(Relation::Behind { commits: 1 })))
    );
}

/// The JSON carries the facts as the contract spells them, every field
/// present — `null`, never omitted — and the text summary counts the quiet
/// entries by them: at rest, on another branch, or pinned.
#[test]
fn the_json_and_the_summary_counts() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.assert_head(&app, Some("main"));
    ws.assert_track(&app, "main", "");
    ws.assert_clean(&app);
    ws.remote("feat", &[]);
    ws.upstream_commit("feat", "feat");
    ws.declare_repo("feat", "feat", "");
    let feat = ws.clone_owned("feat", "feat", &[]);
    ws.git(&feat, &["checkout", "-q", "feat"]);
    ws.assert_head(&feat, Some("feat"));
    ws.assert_track(&feat, "feat", "");
    ws.assert_clean(&feat);
    ws.remote("oracle", &[]);
    ws.declare_reference("oracle", THIRD_PARTY, "oracle", "pinned = true");
    let oracle = ws.clone_third_party("oracle", "oracle", &[]);
    ws.assert_head(&oracle, Some("main"));
    ws.assert_clean(&oracle);
    ws.remote("spec", &[]);
    ws.declare_reference("spec", THIRD_PARTY, "spec", "");
    let spec = ws.clone_third_party("spec", "spec", &[]);
    ws.git(&spec, &["checkout", "-q", "--detach"]);
    ws.assert_head(&spec, None);
    ws.assert_clean(&spec);
    ws.declare_repo("gone", "gone", "");
    assert!(!ws.dir("gone").exists());
    ws.write_registry();

    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], "sync would    clone gone");
    assert!(
        lines[1].starts_with("clean 2 · on branches 1 · pinned 1 "),
        "{text}"
    );

    let report: Value =
        serde_json::from_str(&stdout(&repos(&ws, &ws.root(), &["status", "--json"]))).unwrap();
    let at_rest = |key: &str| {
        report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == key)
            .unwrap()["at_rest"]
            .clone()
    };
    assert_eq!(
        at_rest("app"),
        json!({"on_branch": true, "clean": true, "idle": true, "followed": {"kind": "in_sync"}})
    );
    assert_eq!(
        at_rest("feat"),
        json!({"on_branch": false, "clean": true, "idle": true, "followed": {"kind": "in_sync"}})
    );
    assert_eq!(
        at_rest("oracle"),
        json!({"on_branch": null, "clean": true, "idle": true, "followed": null})
    );
    assert_eq!(
        at_rest("spec"),
        json!({"on_branch": null, "clean": true, "idle": true, "followed": null})
    );
    assert_eq!(at_rest("gone"), Value::Null);
}
