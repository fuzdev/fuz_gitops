//! The `repos` binary over a fixture workspace: exit codes, the `--json`
//! document, and the global `--registry` / `--root` flags. Run under the
//! same hermetic environment as the fixtures.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::process::Output;

use fuz_repos::STATUS_FORMAT_VERSION;
use serde_json::Value;
use support::FixtureWorkspace;

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

fn repos(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> Output {
    ws.command(REPOS, cwd).args(args).output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// A workspace with `app` ahead by one and `gone` missing.
fn workspace() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.declare_repo("gone", "gone", "");
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    ws
}

fn parse(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap()
}

#[test]
fn status_json_is_the_versioned_report() {
    let ws = workspace();
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(report["version"], STATUS_FORMAT_VERSION);
    assert_eq!(report["workspace"], ws.root().to_str().unwrap());
    assert_eq!(report["unregistered"], Value::Null);
    let entries = report["entries"].as_array().unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["app", "gone"]);
    assert_eq!(entries[0]["presence"]["kind"], "present");
    assert_eq!(entries[0]["branches"][0]["verdict"]["kind"], "act");
    assert_eq!(
        entries[0]["branches"][0]["verdict"]["action"],
        serde_json::json!({"kind": "push", "commits": 1})
    );
    assert_eq!(entries[1]["presence"]["kind"], "missing");
}

#[test]
fn status_targets_from_inside_a_checkout() {
    let ws = workspace();
    let report = parse(&repos(&ws, &ws.dir("app"), &["status", "--json", "."]));
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["key"], "app");
}

#[test]
fn status_from_a_linked_worktree_outside_the_workspace() {
    let ws = workspace();
    let app = ws.dir("app");
    // `feature` merged and deleted upstream, its clean worktree removable;
    // `scratch` dirty
    let feature = ws.outside("app-feature");
    ws.add_worktree(&app, &feature, &["-b", "feature"]);
    ws.git(&feature, &["push", "-q", "-u", "origin", "feature"]);
    ws.upstream_delete_branch("app", "feature");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "feature", "[gone]");
    ws.assert_clean(&feature);
    let scratch = ws.outside("app-scratch");
    ws.add_worktree(&app, &scratch, &["-b", "scratch"]);
    support::write(&scratch, "notes.txt", "x\n");
    ws.assert_porcelain(&scratch, &["?? notes.txt"]);
    // and one deleted by hand
    let gone = ws.outside("app-gone");
    ws.add_worktree(&app, &gone, &["-b", "gone"]);
    std::fs::remove_dir_all(&gone).unwrap();
    for wt in [&feature, &scratch, &gone] {
        assert!(!wt.starts_with(ws.root()));
    }

    // outside the workspace, discovery can't walk up to the registry
    let registry = ws.root().join("repos.toml");
    let registry = registry.to_str().unwrap();
    let report = parse(&repos(
        &ws,
        &feature,
        &["--registry", registry, "status", "--json", "."],
    ));
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e["key"], "app");
    assert_eq!(
        e["unprobed_worktrees"],
        serde_json::json!([{
            "path": gone.to_str().unwrap(),
            "head": {"kind": "branch", "name": "gone"},
            "locked": false,
            "in_progress": null,
            "why": {"kind": "prunable"},
            "prune": {"kind": "safe"},
        }])
    );
    let checkouts = e["checkouts"].as_array().unwrap();
    let paths: Vec<(&str, bool)> = checkouts
        .iter()
        .map(|c| (c["path"].as_str().unwrap(), c["primary"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        paths,
        [
            (app.to_str().unwrap(), true),
            (feature.to_str().unwrap(), false),
            (scratch.to_str().unwrap(), false),
        ]
    );
    assert_eq!(checkouts[2]["uncommitted"]["untracked"], 1);
    assert_eq!(checkouts[1]["locked"], false);
    assert_eq!(checkouts[1]["linked"], true);
    assert_eq!(checkouts[0]["linked"], false);
    // checked for the clean worktree that could be removed; not otherwise
    assert_eq!(checkouts[1]["submodules"], false);
    assert_eq!(checkouts[2]["submodules"], Value::Null);
    assert_eq!(checkouts[0]["submodules"], Value::Null);
    let branch = e["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "feature")
        .unwrap();
    assert_eq!(
        branch["verdict"],
        serde_json::json!({
            "kind": "cleanup",
            "reason": "upstream_gone",
            "removable_worktree": feature.to_str().unwrap(),
        })
    );

    let out = repos(&ws, &feature, &["--registry", registry, "status", "."]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "uncommitted   app (worktree {}, 1)\n",
            scratch.display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            // main's local commit is on it too, on no remote
            "cleanup       app:feature (upstream gone, +1, worktree {} removable)  \
             app (worktree {} gone — git worktree prune, or git worktree repair <new path> \
             if it moved)\n",
            feature.display(),
            gone.display()
        )),
        "{text}"
    );
}

#[test]
fn status_text_exits_zero_whatever_it_reports() {
    let ws = workspace();
    for args in [&["status"][..], &["status", "--verbose"]] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        let text = stdout(&out);
        assert!(text.contains("app"), "{text}");
        assert!(text.contains("gone"), "{text}");
    }
}

#[test]
fn a_registry_outside_the_workspace_with_root() {
    let ws = workspace();
    let meta = ws.outside("meta");
    std::fs::create_dir(&meta).unwrap();
    let registry = meta.join("repos.toml");
    std::fs::rename(ws.root().join("repos.toml"), &registry).unwrap();

    // without --root, the registry's dir is the root and every entry is missing
    let report = parse(&repos(
        &ws,
        &ws.root(),
        &["--registry", registry.to_str().unwrap(), "status", "--json"],
    ));
    assert_eq!(report["workspace"], meta.to_str().unwrap());
    assert_eq!(report["entries"][0]["presence"]["kind"], "missing");

    let report = parse(&repos(
        &ws,
        &meta,
        &[
            "--registry",
            "repos.toml",
            "--root",
            ws.root().to_str().unwrap(),
            "status",
            "--json",
        ],
    ));
    assert_eq!(report["workspace"], ws.root().to_str().unwrap());
    assert_eq!(report["registry"], registry.to_str().unwrap());
    assert_eq!(report["entries"][0]["presence"]["kind"], "present");
}

#[test]
fn caller_errors_exit_two() {
    let ws = workspace();
    let cases: [(&[&str], &str); 4] = [
        (&["status", "nope"], "no registry entry matches `nope`"),
        (&["--root", "nowhere", "status"], "no workspace root"),
        (
            &["--registry", "missing.toml", "status"],
            "failed to read the registry",
        ),
        (&[], "a subcommand is required"),
    ];
    for (args, message) in cases {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
    // an argument the parser rejects
    let out = repos(&ws, &ws.root(), &["status", "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn an_invalid_registry_exits_two() {
    let ws = workspace();
    std::fs::write(
        ws.root().join("repos.toml"),
        "owners = [\"me\"]\n[repos.app]\nurl = \"https://github.com/me/app\"\nbogus = 1\n",
    )
    .unwrap();
    let out = repos(&ws, &ws.root(), &["status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("bogus"), "{}", stderr(&out));
}

#[test]
fn version_and_help_exit_zero() {
    let ws = workspace();
    let out = repos(&ws, &ws.root(), &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).starts_with(&format!("repos {}", env!("CARGO_PKG_VERSION"))),
        "{}",
        stdout(&out)
    );
    let out = repos(&ws, &ws.root(), &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("status"));
}
