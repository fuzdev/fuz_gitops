//! The `repos status` documents and text over a fixture workspace: the
//! versioned report, fetching, credentials, targets, and text output. Run under
//! the same hermetic environment as the fixtures.

mod support;

use fuz_repos::STATUS_FORMAT_VERSION;
use serde_json::Value;
use support::FixtureWorkspace;
use support::cli::{REPOS, parse, repos, stderr, stdout, workspace};

#[test]
fn status_json_is_the_versioned_report() {
    let ws = workspace();
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(report["version"], STATUS_FORMAT_VERSION);
    assert_eq!(report["workspace"], ws.root().to_str().unwrap());
    // the scan ran and found nothing
    assert_eq!(report["unregistered"], serde_json::json!([]));
    // the fixture's HOME records no Claude Code session: nothing live
    assert_eq!(
        report["sessions"],
        serde_json::json!({"kind": "available", "unscoped": []})
    );
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
    assert_eq!(
        entries[1]["clone"],
        serde_json::json!({
            "kind": "act",
            "recipe": {
                "url": "git@github.com:me/gone",
                "branch": "main",
                "shallow": false,
                "sparse": null,
            },
        })
    );
    assert_eq!(entries[0]["clone"], Value::Null);
}

#[test]
fn status_fetch_is_recorded_and_checks_private_repos() {
    let mut ws = workspace();
    // declared at a loopback host nothing serves, and never cloned (the
    // check reads the host alone): were the allowlist to fail, the read
    // would stop at this machine, never reach the network
    ws.declare_repo_url("secret", "https://127.0.0.1/me/secret", "private");
    ws.write_registry();

    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(report["fetched"], false);
    for e in report["entries"].as_array().unwrap() {
        assert_eq!(e["fetch_error"], Value::Null, "{}", e["key"]);
        assert_eq!(e["visibility_check"], Value::Null, "{}", e["key"]);
    }

    let report = parse(&repos(&ws, &ws.root(), &["status", "--json", "--fetch"]));
    assert_eq!(report["fetched"], true);
    let entries = report["entries"].as_array().unwrap();
    let entry = |key: &str| entries.iter().find(|e| e["key"] == key).unwrap();
    assert_eq!(entry("app")["fetch_error"], Value::Null);
    assert_eq!(entry("app")["visibility_check"], Value::Null);
    // the binary reads the registry's https URL; the fixture's protocol
    // allowlist (`file` only) stands, so git refuses it before it leaves
    // the process — the check ran
    assert_eq!(
        entry("secret")["visibility_check"],
        serde_json::json!({
            "kind": "unknown",
            "failure": {"kind": "failed", "message": "fatal: transport 'https' not allowed"}
        })
    );

    let out = repos(&ws, &ws.root(), &["status", "--fetch"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "failed        secret (visibility check: fatal: transport 'https' not allowed)\n"
        ),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_registry_url_with_credentials_is_refused_unrepeated() {
    let mut ws = workspace();
    ws.declare_repo_url(
        "leaky",
        "https://user:sekrit@github.com/me/leaky",
        "private",
    );
    ws.write_registry();
    for args in [&["status", "--json"][..], &["status"]] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let all = format!("{}{}", stdout(&out), stderr(&out));
        assert!(all.contains("carries credentials"), "{all}");
        assert!(!all.contains("sekrit") && !all.contains("user:"), "{all}");
    }
}

#[test]
fn a_credential_in_an_origin_url_never_prints() {
    let ws = workspace();
    // a registered entry and an unregistered clone, each with a token in
    // its origin
    let app = ws.dir("app");
    let token_url = "https://me:ghp_TOKEN@github.com/old/app";
    ws.set_origin(&app, "app", token_url);
    ws.remote("stray", &[]);
    ws.clone_as(
        "stray",
        "stray",
        "https://ghp_OTHER@github.com/me/stray",
        &[],
    );
    ws.write_registry();

    for args in [
        &["status"][..],
        &["status", "--verbose"],
        &["status", "--json"],
        &["status", "--json", "app"],
    ] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        let all = format!("{}{}", stdout(&out), stderr(&out));
        assert!(!all.contains("ghp_"), "{args:?}: {all}");
        assert!(all.contains("***@github.com"), "{args:?}: {all}");
    }
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(
        report["unregistered"][0]["origin"],
        "https://***@github.com/me/stray"
    );
    // redacted for show, still read as owned
    assert_eq!(report["unregistered"][0]["owned"], true);
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
    // backdated, so the text's `fetched … ago` reads the same across runs
    support::set_mtime(
        &app.join(".git/FETCH_HEAD"),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(support::CLOCK_START),
    );
    ws.assert_track(&app, "feature", "[gone]");
    ws.assert_clean(&feature);
    let scratch = ws.outside("app-scratch");
    ws.add_worktree(&app, &scratch, &["-b", "scratch"]);
    support::write(&scratch, "notes.txt", "x\n");
    ws.assert_porcelain(&scratch, &["?? notes.txt"]);
    // and one deleted by hand
    let gone = ws.outside("app-gone");
    let gone_admin = ws.add_worktree(&app, &gone, &["-b", "gone"]);
    std::fs::remove_dir_all(&gone).unwrap();
    for wt in [&feature, &scratch, &gone] {
        assert!(!wt.starts_with(ws.root()));
    }

    // outside the workspace, the walk-up from the cwd finds no registry: it
    // walks up again from the repo's main checkout — from the worktree's
    // root or deeper — or `--registry` names it
    let registry = ws.root().join("repos.toml");
    let registry = registry.to_str().unwrap();
    let deeper = feature.join("src/deeper");
    std::fs::create_dir_all(&deeper).unwrap();
    let report = parse(&repos(&ws, &feature, &["status", "--json", "."]));
    assert_eq!(report["workspace"], ws.root().to_str().unwrap());
    assert_eq!(report["registry"], registry);
    assert_eq!(report["entries"][0]["fetched_at"], support::CLOCK_START);
    for (cwd, args) in [
        (&deeper, &["status", "--json", "."][..]),
        (&feature, &["--registry", registry, "status", "--json", "."]),
    ] {
        assert_eq!(parse(&repos(&ws, cwd, args)), report, "{args:?}");
    }
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e["key"], "app");
    assert_eq!(
        e["unprobed_worktrees"],
        serde_json::json!([{
            "path": gone.to_str().unwrap(),
            "git_dir": gone_admin.to_str().unwrap(),
            "head": {"kind": "branch", "name": "gone"},
            "holds": {"submodules": false, "worktree_refs": false, "staged": false},
            "locked": false,
            "in_progress": null,
            "why": {"kind": "prunable"},
            "prune": {"kind": "safe"},
            "busy": [],
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

    let out = repos(&ws, &feature, &["status", "."]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let with_registry = repos(&ws, &feature, &["--registry", registry, "status", "."]);
    assert_eq!(stdout(&with_registry), text);
    assert!(
        text.contains(&format!(
            "uncommitted   app (worktree {}, 1)\n",
            scratch.display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            // main's local commit is on it too, on no remote; the second item
            // is too long to share a line, so it hangs below the first
            "cleanup       app:feature (upstream gone, +1, worktree {} removable)\n              \
             app (worktree {gone} gone — if it moved, move it back (or to the workspace root) \
             and rerun repos status, else git -C {app} worktree remove {gone})\n",
            feature.display(),
            gone = gone.display(),
            app = app.display(),
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
fn piped_output_is_never_colored() {
    let ws = workspace();
    // `NO_COLOR` unset (the environment is cleared): only the pipe keeps
    // color off
    for args in [
        &["status"][..],
        &["status", "--verbose"],
        &["status", "--json"],
    ] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        let text = stdout(&out);
        assert!(
            text.contains("sync would") || text.contains("\"version\""),
            "{text}"
        );
        assert!(!text.contains('\x1b'), "{text:?}");
        assert!(!stderr(&out).contains('\x1b'));
    }
}

#[test]
fn the_summary_wraps_at_columns() {
    let mut ws = workspace();
    ws.declare_repo("other", "other", "");
    ws.write_registry();
    let status = |columns: Option<&str>| {
        let mut cmd = ws.command(REPOS, &ws.root());
        cmd.arg("status");
        if let Some(columns) = columns {
            cmd.env("COLUMNS", columns);
        }
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        stdout(&out)
    };
    let one_line = "sync would    push app +1 · clone gone, other\n";
    // unset, unusable, or too narrow: 100
    for columns in [None, Some("wide"), Some("39")] {
        let text = status(columns);
        assert!(text.starts_with(one_line), "{columns:?}: {text}");
    }
    let text = status(Some("40"));
    assert!(
        text.starts_with("sync would    push app +1\n              clone gone, other\n"),
        "{text}"
    );
}

#[test]
fn a_worktree_another_entry_uses_is_kept_whatever_the_targets() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // `old`, its upstream gone, checked out in a clean linked worktree that
    // is itself a registry entry's dir
    ws.git(&app, &["branch", "-q", "old", "main"]);
    ws.git(&app, &["push", "-q", "-u", "origin", "old"]);
    ws.upstream_delete_branch("app", "old");
    ws.git(&app, &["fetch", "-q", "--prune", "origin"]);
    ws.assert_track(&app, "old", "[gone]");
    let old = ws.dir("app-old");
    ws.add_worktree(&app, &old, &["old"]);
    ws.assert_clean(&old);
    ws.declare_repo("app_old", "app", "dir = \"app-old\"");
    ws.write_registry();

    // the other entry isn't a target, and its dir still counts
    for args in [&["status", "--json"][..], &["status", "--json", "app"]] {
        let report = parse(&repos(&ws, &ws.root(), args));
        let branches = report["entries"][0]["branches"].as_array().unwrap();
        let old = branches.iter().find(|b| b["name"] == "old").unwrap();
        assert_eq!(
            old["verdict"],
            serde_json::json!({
                "kind": "cleanup",
                "reason": "upstream_gone",
                "removable_worktree": null,
            }),
            "{args:?}"
        );
    }
}
