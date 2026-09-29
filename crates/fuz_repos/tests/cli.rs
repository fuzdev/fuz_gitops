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
