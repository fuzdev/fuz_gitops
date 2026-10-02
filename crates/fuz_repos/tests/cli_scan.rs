//! The unregistered-clone scan and the repair blocks it prints, through the
//! binary. Run under the same hermetic environment as the fixtures.

mod support;

use serde_json::Value;
use support::cli::{parse, repos, stderr, stdout, workspace};
use support::unregistered::{assert_listed, copy_dir, gitdir_file, points_at};

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

#[test]
fn a_gone_worktree_is_never_told_to_repair() {
    let ws = workspace();
    let app = ws.dir("app");
    // `wa` moved out of its dir by hand, and `wb` moved into it
    let wa = ws.outside("wa");
    let wa_git_dir = ws.add_worktree(&app, &wa, &["-b", "wa"]);
    let wb = ws.dir("wb");
    let wb_git_dir = ws.add_worktree(&app, &wb, &["-b", "wb"]);
    std::fs::rename(&wa, ws.outside("wa-old")).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    assert_eq!(points_at(&wa), "wb");
    // each git dir still names the path its worktree was added at
    let dot_git = |path: &std::path::Path| path.join(".git").to_str().unwrap().to_owned();
    assert_eq!(gitdir_file(&wa_git_dir), dot_git(&wa));
    assert_eq!(gitdir_file(&wb_git_dir), dot_git(&wb));
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

    // the repair one might reach for, at `wb`'s new path: git repoints
    // `wb`'s git dir there, where `wa`'s still names its worktree, so two
    // git dirs claim the one checkout
    ws.git(&app, &["worktree", "repair", wa.to_str().unwrap()]);
    assert_eq!(gitdir_file(&wa_git_dir), dot_git(&wa));
    assert_eq!(gitdir_file(&wb_git_dir), dot_git(&wa));
    assert_listed(&ws, &app, &[(&wa, false), (&wa, false), (&spike, true)]);
    // which of them the checkout's `.git` names now is the filesystem's:
    // git walks `worktrees/` in the order it lists them, each git dir
    // naming the path rewrites that `.git` in turn, and the last stays —
    // when that's `wa`'s, `wb`'s checkout is handed to `wa`
    let kept = points_at(&wa);
    assert!(["wa", "wb"].contains(&kept.as_str()), "{kept}");
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
