//! The `repos` binary over a fixture workspace: exit codes, the `--json`
//! documents (the report and the fatal-error document), discovery, and the
//! global `--registry` / `--root` flags. Run under the same hermetic
//! environment as the fixtures.

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
    // the scan ran and found nothing
    assert_eq!(report["unregistered"], serde_json::json!([]));
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
            // main's local commit is on it too, on no remote
            "cleanup       app:feature (upstream gone, +1, worktree {} removable)  \
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
fn status_scans_for_unregistered_dirs_only_over_the_whole_workspace() {
    let ws = workspace();
    let app = ws.dir("app");
    let feat = ws.dir("app-feat");
    ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let moved = ws.dir("app-moved");
    std::fs::rename(&feat, &moved).unwrap();

    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    assert_eq!(
        report["unregistered"],
        serde_json::json!([{
            "dir": "app-moved",
            "origin": support::owned_origin("app"),
            "owned": true,
            "kind": "moved_worktree",
            "entry": "app",
            "blocked_by": null,
            "exit_noise": null,
        }])
    );
    // with targets the report is about them alone: the scan didn't run
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json", "app"]));
    assert_eq!(report["unregistered"], Value::Null);

    let out = repos(&ws, &ws.root(), &["status"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(
            "unregistered  owned: app-moved (moved worktree of app — git worktree repair)\n"
        ),
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["status", "--verbose"]);
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "app-moved  unregistered · owned · moved worktree of app\n  \
             dir       {moved}\n  \
             origin    git@github.com:me/app\n  \
             fix       git -C {app} worktree repair {moved}\n",
            moved = moved.display(),
            app = app.display(),
        )),
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["status", "app"]);
    assert!(!stdout(&out).contains("unregistered"), "{}", stdout(&out));
}

/// `cp -r from to`: a copy that keeps every file, `.git` included.
fn copy_dir(ws: &FixtureWorkspace, from: &Path, to: &Path) {
    let out = ws
        .command("cp", ws.base())
        .args([std::ffi::OsStr::new("-r"), from.as_os_str(), to.as_os_str()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// Where a checkout's `.git` file points, by the git dir's name.
fn points_at(checkout: &Path) -> String {
    let target = std::fs::read_to_string(checkout.join(".git")).unwrap();
    Path::new(target.trim_end())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn a_gone_worktree_is_never_told_to_repair() {
    let ws = workspace();
    let app = ws.dir("app");
    // `wa` moved out of its dir by hand, and `wb` moved into it
    let wa = ws.outside("wa");
    ws.add_worktree(&app, &wa, &["-b", "wa"]);
    let wb = ws.dir("wb");
    ws.add_worktree(&app, &wb, &["-b", "wb"]);
    std::fs::rename(&wa, ws.outside("wa-old")).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    assert_eq!(points_at(&wa), "wb");
    ws.assert_head(&wa, Some("wb"));
    assert!(!wb.exists());
    // and a detached one deleted, which removing it would lose
    let spike = ws.outside("spike");
    ws.add_worktree(&app, &spike, &["--detach"]);
    std::fs::remove_dir_all(&spike).unwrap();

    for args in [&["status"][..], &["status", "--verbose"]] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        let text = stdout(&out);
        assert!(
            text.contains(&format!(
                "app (worktree {wb} gone — if it moved, move it back (or to the workspace root) \
                 and rerun repos status, else git -C {app} worktree remove {wb})",
                wb = wb.display(),
                app = app.display(),
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "app (worktree {} gone — if it moved, move it back (or to the workspace root) \
                 and rerun repos status; removing discards its detached HEAD)",
                spike.display()
            )),
            "{text}"
        );
        assert!(!text.contains("worktree repair"), "{text}");
    }

    // the repair one might reach for, at `wb`'s new path: git's walk over
    // the other worktree git dirs hands `wb`'s checkout to `wa`
    ws.git(&app, &["worktree", "repair", wa.to_str().unwrap()]);
    assert_eq!(points_at(&wa), "wa");
}

#[test]
fn a_gone_worktree_is_removed_alone() {
    let ws = workspace();
    let app = ws.dir("app");
    let worktrees = app.join(".git/worktrees");
    // `a` deleted; `b` moved to the workspace root, with staged work; `c`
    // deleted, detached
    let a = ws.dir("a");
    ws.add_worktree(&app, &a, &["-b", "a"]);
    std::fs::remove_dir_all(&a).unwrap();
    let b = ws.dir("b");
    ws.add_worktree(&app, &b, &["-b", "b"]);
    let b_moved = ws.dir("b-moved");
    std::fs::rename(&b, &b_moved).unwrap();
    support::write(&b_moved, "p.txt", "precious\n");
    ws.git(&b_moved, &["add", "p.txt"]);
    ws.assert_porcelain(&b_moved, &["A  p.txt"]);
    let c = ws.outside("c");
    ws.add_worktree(&app, &c, &["--detach"]);
    std::fs::remove_dir_all(&c).unwrap();
    for id in ["a", "b", "c"] {
        assert!(worktrees.join(id).is_dir(), "{id}");
    }

    let out = repos(&ws, &ws.root(), &["status"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("b-moved (moved worktree of app — git worktree repair)"),
        "{text}"
    );
    // `b`'s own line points there, with no command: removing it would
    // orphan `b-moved`
    let b_moved_line = format!(
        "app (worktree {} gone — moved to b-moved; see its line)",
        b.display()
    );
    assert!(text.contains(&b_moved_line), "{text}");
    let remove_b = format!("worktree remove {}", b.display());
    assert!(!text.contains(&remove_b), "{text}");
    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    let unprobed = report["entries"][0]["unprobed_worktrees"]
        .as_array()
        .unwrap();
    let b_prune = unprobed
        .iter()
        .find(|u| u["path"] == b.to_str().unwrap())
        .map(|u| &u["prune"]);
    assert_eq!(
        b_prune,
        Some(&serde_json::json!({"kind": "moved", "to": ["b-moved"]}))
    );
    // with targets there's no scan, so the line can't know it moved: the
    // hedge is all it has, and the work staged in `b-moved` (in `b`'s
    // index) keeps it from reading safe
    let text = stdout(&repos(&ws, &ws.root(), &["status", "app"]));
    assert!(
        text.contains(&format!(
            "app (worktree {} gone — if it moved, move it back (or to the workspace root) \
             and rerun repos status; removing discards its staged changes)",
            b.display()
        )),
        "{text}"
    );
    assert!(!text.contains(&remove_b), "{text}");
    assert!(!text.contains("moved to"), "{text}");
    // a copy too: both named, neither offered a repair
    copy_dir(&ws, &b_moved, &ws.dir("b-copy"));
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.contains(&format!(
            "app (worktree {} gone — moved to b-copy, b-moved; see their lines)",
            b.display()
        )),
        "{text}"
    );
    assert!(!text.contains(&remove_b), "{text}");
    std::fs::remove_dir_all(ws.dir("b-copy")).unwrap();

    // the command `a`'s cleanup advises, run as printed
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    let start = format!("app (worktree {} gone — ", a.display());
    let advice = &text[text.find(&start).unwrap_or_else(|| panic!("{text}")) + start.len()..];
    let command = advice.split_once(", else ").unwrap().1;
    let command = &command[..command.find(')').unwrap()];
    assert_eq!(
        command,
        format!("git -C {} worktree remove {}", app.display(), a.display())
    );
    let words: Vec<&str> = command.split(' ').collect();
    assert_eq!(words[0], "git");
    ws.git(ws.base(), &words[1..]);

    // `a`'s git dir alone is gone: `b-moved` keeps its staged work and its
    // repair, and `c` its detached HEAD
    assert!(!worktrees.join("a").exists());
    assert!(worktrees.join("b").is_dir());
    assert!(worktrees.join("c").join("HEAD").is_file());
    ws.assert_porcelain(&b_moved, &["A  p.txt"]);
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.contains("b-moved (moved worktree of app — git worktree repair)"),
        "{text}"
    );
    assert!(text.contains(&b_moved_line), "{text}");
    assert!(
        text.contains(&format!("app (worktree {} gone", c.display())),
        "{text}"
    );
    assert!(
        !text.contains(&format!("worktree {} gone", a.display())),
        "{text}"
    );
}

#[test]
fn a_locked_worktree_moved_into_the_root_is_not_prunable() {
    let ws = workspace();
    let app = ws.dir("app");
    // locked, as on removable media, then moved into the root by hand: git
    // keeps it (missing, not prunable), and the scan finds the moved copy
    let lk = ws.dir("lk");
    ws.add_worktree(&app, &lk, &["-b", "lk"]);
    ws.git(&app, &["worktree", "lock", lk.to_str().unwrap()]);
    std::fs::rename(&lk, ws.dir("lk-moved")).unwrap();
    let record = ws.worktree_record(&app, &lk);
    assert!(record.iter().any(|l| l.starts_with("locked")), "{record:?}");
    assert!(
        !record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    let report = parse(&repos(&ws, &ws.root(), &["status", "--json"]));
    let unprobed = &report["entries"][0]["unprobed_worktrees"];
    assert_eq!(unprobed.as_array().map(Vec::len), Some(1), "{unprobed}");
    // `prune` is set exactly when it's prunable: never `moved` here
    assert_eq!(unprobed[0]["why"]["kind"], "missing");
    assert_eq!(unprobed[0]["prune"], Value::Null);
    let strays = report["unregistered"].as_array().unwrap();
    assert_eq!(strays.len(), 1);
    assert_eq!(strays[0]["dir"], "lk-moved");
    assert_eq!(strays[0]["kind"], "shared_git_dir");
}

#[test]
fn a_workspace_root_that_cannot_be_listed_exits_one() {
    let ws = workspace();
    let root = ws.root();
    let Some(_unseal) = support::seal(&root, 0o311) else {
        return;
    };
    let out = repos(&ws, &root, &["status"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("failed to list the workspace root"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty());
    // with targets there's no scan, and no listing
    let out = repos(&ws, &root, &["status", "app"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
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
    let cases: [(&[&str], &str); 5] = [
        (
            &["status", "nope"],
            "error: unknown target `nope`\nhint: a target is",
        ),
        (
            &["status", "apq"],
            "error: unknown target `apq`\nhint: did you mean: app\n",
        ),
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
    // an argument the parser rejects, `--json` or not: argh's text on
    // stderr, nothing on stdout
    for args in [&["status", "--bogus"][..], &["status", "--json", "--bogus"]] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&out).contains("--bogus"),
            "{args:?}: {}",
            stderr(&out)
        );
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
}

/// A fatal error's `--json` document: exactly `version` and `error`, the
/// message and hint the same ones stderr prints. Returns `error`.
fn error_doc(out: &Output, code: i32) -> Value {
    assert_eq!(out.status.code(), Some(code), "stderr: {}", stderr(out));
    let doc: Value = serde_json::from_str(&stdout(out))
        .map_err(|e| format!("{e}: stdout: {}", stdout(out)))
        .unwrap();
    let fields: Vec<&String> = doc.as_object().unwrap().keys().collect();
    assert_eq!(fields, ["error", "version"], "{doc}");
    assert_eq!(doc["version"], STATUS_FORMAT_VERSION);
    let error = doc["error"].clone();
    let err = stderr(out);
    let message = error["message"].as_str().unwrap();
    assert!(err.starts_with(&format!("error: {message}\n")), "{err}");
    match error["hint"].as_str() {
        Some(hint) => assert!(err.ends_with(&format!("\nhint: {hint}\n")), "{err}"),
        None => assert!(!err.contains("hint:"), "{err}"),
    }
    error
}

/// A `git` on `PATH` ahead of the real one that prints `version` for any
/// call; returns the `PATH` to run under.
fn fake_git(ws: &FixtureWorkspace, version: &str) -> std::ffi::OsString {
    let bin = ws.outside("fake-bin");
    support::write_executable(&bin, "git", &format!("#!/bin/sh\necho '{version}'\n"));
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn caller_errors_print_a_json_document() {
    let ws = workspace();
    let root = ws.root();
    let registry = root.join("repos.toml");

    // an unknown target, with its close keys
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "apq"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "unknown_entry",
            "name": "apq",
            "suggestions": ["app"],
            "message": "unknown target `apq`",
            "hint": "did you mean: app",
        })
    );
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "zzzzzz"]), 2);
    assert_eq!(error["suggestions"], serde_json::json!([]));
    assert_eq!(
        error["hint"],
        "a target is a registry key, an entry's dir name, or a path inside a checkout"
    );

    let error = error_doc(
        &repos(&ws, &root, &["--root", "nowhere", "status", "--json"]),
        2,
    );
    assert_eq!(error["kind"], "root_not_found");
    assert_eq!(
        error["message"],
        format!("no workspace root at {}", root.join("nowhere").display())
    );

    let error = error_doc(
        &repos(
            &ws,
            &root,
            &["--registry", "missing.toml", "status", "--json"],
        ),
        2,
    );
    assert_eq!(error["kind"], "registry_read");
    assert_eq!(error["hint"], Value::Null);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("failed to read the registry at "),
        "{error}"
    );

    // outside the workspace and outside any repo
    let plain = ws.outside("plain");
    std::fs::create_dir(&plain).unwrap();
    let error = error_doc(&repos(&ws, &plain, &["status", "--json"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "registry_not_found",
            "message": format!(
                "no repos.toml found in {} or any parent directory",
                plain.display()
            ),
            "hint": "run inside the workspace or a checkout of one of its repos, or pass \
                     `--registry <path>`",
        })
    );
    // in a repo outside the workspace, whose main checkout has no registry
    // above it either
    ws.remote("stray", &[]);
    let stray = ws.outside("stray");
    ws.git(
        ws.base(),
        &[
            "clone",
            "-q",
            &format!("file://{}", ws.bare("stray").display()),
            stray.to_str().unwrap(),
        ],
    );
    let error = error_doc(&repos(&ws, &stray, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");

    // git missing, or too old for `GIT_NO_LAZY_FETCH`
    let empty = ws.outside("empty-bin");
    std::fs::create_dir(&empty).unwrap();
    let out = ws
        .command(REPOS, &root)
        .env("PATH", &empty)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let error = error_doc(&out, 2);
    assert_eq!(error["kind"], "git_not_found");
    assert_eq!(error["message"], "git not found on PATH");
    // checked before discovery: outside the workspace too
    let out = ws
        .command(REPOS, &plain)
        .env("PATH", &empty)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(error_doc(&out, 2)["kind"], "git_not_found");
    for (version, found) in [
        ("git version 2.40.0", "2.40.0"),
        // a wrapper's banner before the version line
        ("wrapper banner\ngit version 2.40.1", "2.40.1"),
        ("git version 2.43.7 (Apple Git-150)", "2.43.7"),
        ("git version 2.43.0.windows.1", "2.43.0"),
        ("not a git at all", "not a git at all"),
    ] {
        let path = fake_git(&ws, version);
        let out = ws
            .command(REPOS, &root)
            .env("PATH", &path)
            .args(["status", "--json"])
            .output()
            .unwrap();
        let error = error_doc(&out, 2);
        assert_eq!(
            error,
            serde_json::json!({
                "kind": "git_too_old",
                "found": found,
                "required": "2.44.0",
                "message": format!("git reports version `{found}`; repos needs 2.44.0 or newer"),
                "hint": "repos sets `GIT_NO_LAZY_FETCH` (git 2.44+) so a local call on a \
                         partial clone never touches the network — upgrade git",
            }),
            "{version}"
        );
        // text mode: the same lines, nothing on stdout
        let out = ws
            .command(REPOS, &root)
            .env("PATH", &path)
            .args(["status"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(stdout(&out).is_empty());
        assert!(
            stderr(&out).contains("GIT_NO_LAZY_FETCH"),
            "{}",
            stderr(&out)
        );
    }
    // git before 2.15 rejects the runner's `--no-optional-locks` before it
    // can print a version
    let bin = ws.outside("ancient-bin");
    support::write_executable(
        &bin,
        "git",
        "#!/bin/sh\necho \"Unknown option: $1\" >&2\necho 'usage: git [--version] <command>' >&2\n\
         exit 129\n",
    );
    let out = ws
        .command(REPOS, &root)
        .env("PATH", &bin)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let error = error_doc(&out, 2);
    assert_eq!(error["kind"], "git_too_old");
    assert_eq!(error["found"], "unknown (older than 2.15)");
    assert!(!stderr(&out).contains("usage:"), "{}", stderr(&out));
    // a new enough fake passes the check: whatever fails next isn't it
    let out = ws
        .command(REPOS, &root)
        .env("PATH", fake_git(&ws, "git version 2.44.0.windows.1"))
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        !stderr(&out).contains("git reports version"),
        "{}",
        stderr(&out)
    );

    // last: it rewrites the registry
    std::fs::write(
        &registry,
        "owners = [\"me\"]\n[repos.app]\nurl = \"https://github.com/me/app\"\nbogus = 1\n",
    )
    .unwrap();
    let error = error_doc(&repos(&ws, &root, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_parse");
    assert!(
        error["message"].as_str().unwrap().contains("bogus"),
        "{error}"
    );
}

#[test]
fn a_non_utf8_argument_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let ws = workspace();
    let arg = std::ffi::OsStr::from_bytes(b"a\xffb");
    let s = std::ffi::OsStr::new;
    // a target, and a flag's value
    for args in [
        [s("status"), s("--json"), arg],
        [s("--registry"), arg, s("status")],
    ] {
        let out = ws.command(REPOS, &ws.root()).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
        assert!(stdout(&out).is_empty());
        assert_eq!(
            stderr(&out),
            "error: argument `a\u{fffd}b` is not valid UTF-8; repos takes UTF-8 arguments \
             only (paths included)\n"
        );
    }
}

#[test]
fn the_discovery_fallback_is_for_linked_worktrees_only() {
    let ws = workspace();
    // a checkout whose git dir lives in a dir holding a registry: the git
    // dir's parent is no checkout, and discovery doesn't search it
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::copy(ws.root().join("repos.toml"), elsewhere.join("repos.toml")).unwrap();
    let home = ws.outside("home2");
    let git_dir = elsewhere.join("dot.git");
    ws.git(
        ws.base(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", git_dir.display()),
            home.to_str().unwrap(),
        ],
    );
    ws.git(&home, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let proj = home.join("proj");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(
        ws.git(
            &proj,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        git_dir.to_str().unwrap()
    );
    let error = error_doc(&repos(&ws, &proj, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // nor from a linked worktree of it: its common dir isn't a `.git` in a
    // main checkout
    let wt = ws.outside("home2-wt");
    ws.add_worktree(&home, &wt, &["-b", "wt"]);
    let error = error_doc(&repos(&ws, &wt, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // nor when the separate git dir is itself named `.git`: it isn't a
    // linked worktree, so no fallback, though the common dir's parent looks
    // like a checkout
    let elsewhere3 = ws.outside("elsewhere3");
    std::fs::create_dir(&elsewhere3).unwrap();
    std::fs::copy(ws.root().join("repos.toml"), elsewhere3.join("repos.toml")).unwrap();
    let home3 = ws.outside("home3");
    ws.git(
        ws.base(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", elsewhere3.join(".git").display()),
            home3.to_str().unwrap(),
        ],
    );
    let proj3 = home3.join("proj");
    std::fs::create_dir(&proj3).unwrap();
    let error = error_doc(&repos(&ws, &proj3, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // control: from the registry's own dir it's found
    let out = repos(&ws, &elsewhere, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

#[test]
fn a_runtime_error_prints_a_json_document() {
    let ws = workspace();
    let root = ws.root();
    let Some(_unseal) = support::seal(&root, 0o311) else {
        return;
    };
    let error = error_doc(&repos(&ws, &root, &["status", "--json"]), 1);
    assert_eq!(error["kind"], "io");
    assert_eq!(error["hint"], Value::Null);
    assert!(
        error["message"].as_str().unwrap().starts_with(&format!(
            "failed to list the workspace root {}: ",
            root.display()
        )),
        "{error}"
    );
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
fn an_integrity_issue_is_a_registry_invalid_document() {
    let ws = workspace();
    let root = ws.root();
    let registry = root.join("repos.toml");
    std::fs::write(
        &registry,
        r#"owners = ["me"]
[repos.app]
url = "https://github.com/me/app"
visibility = "public"
purpose = "x"
requires = ["spec"]
[repos.theirs]
url = "https://github.com/them/theirs"
dir = "app"
visibility = "public"
purpose = "x"
"#,
    )
    .unwrap();
    let message = format!(
        "invalid registry at {}:\n  \
         repo `theirs` sits under `them`, not an owner — a third-party clone belongs in \
         [references]\n  \
         repo `theirs` claims dir `app`, already claimed by repo `app`\n  \
         repo `app` requires `spec`, which is neither a repo nor a reference",
        registry.display()
    );
    let hint = "fix each issue in the registry; nothing is probed until it validates";
    // validated before targets resolve: the unknown one is never looked at
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "nope"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "registry_invalid",
            "issues": [
                {"kind": "repo_not_owned", "key": "theirs", "account": "them"},
                {
                    "kind": "dir_claimed_twice",
                    "dir": "app",
                    "first": {"kind": "repo", "key": "app"},
                    "second": {"kind": "repo", "key": "theirs"},
                },
                {
                    "kind": "unknown_checkout_ref",
                    "key": "app",
                    "field": "requires",
                    "target": "spec",
                },
            ],
            "message": message,
            "hint": hint,
        })
    );
    // text: every issue on stderr, nothing on stdout
    let out = repos(&ws, &root, &["status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert_eq!(stderr(&out), format!("error: {message}\nhint: {hint}\n"));
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
