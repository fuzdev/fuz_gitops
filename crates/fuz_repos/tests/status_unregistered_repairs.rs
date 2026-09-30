//! The repair blocks the scan offers for moved and swapped worktrees, and the
//! repairs it refuses to offer.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::path::{Path, PathBuf};

use fuz_repos::report::{RepairBlock, UnregisteredKind};
use support::unregistered::{
    app, copy_dir, gitdir_file, moved, moved_by_hand, moved_relative, moved_rewrites, points_at,
    shared, shared_unnamed, stray,
};
use support::{FixtureWorkspace, assert_git_dir_unchanged, owned_origin, snapshot_git_dir};

/// Moved, repairable, but git will complain about `noise` and exit 1.
fn moved_noisy(entry: &str, noise: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: None,
        exit_noise: Some(noise.to_str().unwrap().into()),
    }
}

/// Moved, but the worktree git dir `git_dir` names this very dir.
fn moved_claimed(entry: &str, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::ClaimedDir {
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

#[test]
fn swapped_worktrees_are_repaired_one_safe_step_at_a_time() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.dir("app-feat");
    ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let new = ws.dir("app-new");
    ws.add_worktree(&app, &new, &["-b", "new"]);
    support::write(&new, "n.txt", "new work\n");
    ws.git(&new, &["add", "n.txt"]);
    // renamed by hand: app-feat to app-old, then app-new to app-feat
    let old = ws.dir("app-old");
    std::fs::rename(&feat, &old).unwrap();
    std::fs::rename(&new, &feat).unwrap();
    assert_eq!(points_at(&old), "app-feat");
    assert_eq!(points_at(&feat), "app-new");
    let git_dir = |id: &str| app.join(".git/worktrees").join(id);
    assert_eq!(
        gitdir_file(&git_dir("app-feat")),
        feat.join(".git").to_str().unwrap()
    );

    // git dir `app-feat` names the path `app-feat` now holds, whose `.git`
    // names `app-new`: repairing that one first would take `app-feat`'s
    // git dir for it; app-old's repair is safe
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "app-feat",
                Some(&origin),
                true,
                moved_claimed("app", &git_dir("app-feat"))
            ),
            stray("app-old", Some(&origin), true, moved("app")),
        ]
    );

    ws.git(&app, &["worktree", "repair", old.to_str().unwrap()]);
    assert_eq!(points_at(&old), "app-feat");
    assert_eq!(points_at(&feat), "app-new");
    ws.assert_head(&old, Some("feat"));
    ws.assert_head(&feat, Some("new"));
    ws.assert_porcelain(&feat, &["A  n.txt"]);
    // the next run offers the other
    assert_eq!(
        ws.unregistered(),
        [stray("app-feat", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", feat.to_str().unwrap()]);
    ws.assert_head(&feat, Some("new"));
    ws.assert_porcelain(&feat, &["A  n.txt"]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_repair_that_would_rewrite_another_checkout_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `q` names `<root>/q`, which now holds the worktree of git dir
    // `q2` (swapped by hand, the original deleted)
    let q = ws.dir("q");
    let q_git_dir = ws.add_worktree(&app, &q, &["-b", "y"]);
    let q2 = ws.dir("q2");
    ws.add_worktree(&app, &q2, &["-b", "y2"]);
    std::fs::remove_dir_all(&q).unwrap();
    std::fs::rename(&q2, &q).unwrap();
    assert_eq!(points_at(&q), "q2");
    // an unrelated worktree, moved
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("q", Some(&origin), true, moved_claimed("app", &q_git_dir)),
            stray(
                "s-moved",
                Some(&origin),
                true,
                moved_rewrites("app", &q, &q_git_dir)
            ),
        ]
    );

    // what the refused repair would do: take git dir `q` for `q`'s checkout
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(points_at(&q), "q");
}

#[test]
fn a_repair_that_would_write_into_a_plain_dir_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a dir that's now plain files, no `.git`
    let else_dir = ws.outside("else");
    std::fs::create_dir(&else_dir).unwrap();
    let y = else_dir.join("y");
    let y_git_dir = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    support::write(&y, "notes.txt", "mine\n");
    // and one git would only complain about: a blocked repair carries no
    // noise
    let z = else_dir.join("z");
    ws.add_worktree(&app, &z, &["-b", "z"]);
    std::fs::remove_dir_all(&z).unwrap();
    std::fs::write(&z, "a file\n").unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &y, &y_git_dir)
        )]
    );

    // what the refused repair would do: write a `.git` into it (exiting 1,
    // over `z`)
    ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(y.join(".git").is_file());
}

#[test]
fn a_git_link_into_a_moved_worktrees_git_dir_is_not_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let far = ws.outside("far");
    std::fs::create_dir(&far).unwrap();
    let git_dir = ws.add_worktree(&app, &far.join("wf"), &["-b", "wf"]);
    let link = ws.dir("link");
    std::fs::create_dir(&link).unwrap();
    symlink(&git_dir, link.join(".git")).unwrap();
    std::fs::remove_dir_all(&far).unwrap();
    ws.assert_head(&link, Some("wf"));
    // git won't repair through a `.git` that isn't a file
    ws.git_fails(&app, &["worktree", "repair", link.to_str().unwrap()]);

    assert_eq!(
        ws.unregistered(),
        [stray(
            "link",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn copies_of_a_main_checkout_git_cannot_find_share_its_git_dir() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    // the main checkout keeps its git dir outside the workspace, and the
    // registry's dir is a linked worktree of it
    let git_dir = ws.outside("app.git");
    let main = ws.clone_owned(
        "app-main",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    let app = ws.dir("app");
    ws.add_worktree(&main, &app, &["-b", "feat"]);
    assert_eq!(
        ws.git(
            &app,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        git_dir.to_str().unwrap()
    );
    // alone, nothing says which checkout the git dir is the main one of
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-main",
            Some(&origin),
            true,
            UnregisteredKind::Clone
        )]
    );

    let copy = ws.dir("app-main-copy");
    copy_dir(&ws, &main, &copy);
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-main", Some(&origin), true, shared("app", &copy)),
            stray("app-main-copy", Some(&origin), true, shared("app", &main)),
        ]
    );
}

#[test]
fn a_named_path_holding_a_git_dir_is_left_alone_by_a_repair() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a dir that now holds a repo of its own: git skips a
    // `.git` that isn't a file
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    ws.git(
        ws.base(),
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "-q",
            y.to_str().unwrap(),
        ],
    );
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // the repair reconnects it, complaining about `y` (exit 1) and leaving
    // it be
    let before = snapshot_git_dir(&y.join(".git"));
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".git is not a file"), "{stderr}");
    assert_git_dir_unchanged(&before, &snapshot_git_dir(&y.join(".git")));
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_git_is_a_link_is_not_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // its `.git` a link to a gitfile kept elsewhere
    let x = ws.dir("x");
    ws.add_worktree(&app, &x, &["-b", "x"]);
    let store = ws.outside("store");
    std::fs::create_dir(&store).unwrap();
    std::fs::rename(x.join(".git"), store.join("x.gitfile")).unwrap();
    symlink(store.join("x.gitfile"), x.join(".git")).unwrap();
    ws.assert_head(&x, Some("x"));
    // live, it's skipped
    assert_eq!(ws.unregistered(), []);

    // moved: a repair would write the link's target into the git dir
    std::fs::rename(&x, ws.dir("x-moved")).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "x-moved",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_gitdir_without_a_git_suffix_names_the_dir_itself() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    // git dir `k` names `<app>/sub` by hand: git takes it as the worktree
    // itself, not `<app>`
    let k = ws.outside("k");
    let k_git_dir = ws.add_worktree(&app, &k, &["-b", "k"]);
    let sub = app.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(k_git_dir.join("gitdir"), format!("{}\n", sub.display())).unwrap();
    assert_eq!(
        ws.worktree_record(&app, &sub)[0],
        format!("worktree {}", sub.display())
    );

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &sub, &k_git_dir)
        )]
    );
    // what the refused repair would do: write a `.git` into the main checkout
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(sub.join(".git").is_file());
}

#[test]
fn a_hazard_git_that_is_broken_or_a_link_is_judged_as_git_does() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    // `h` elsewhere, its `.git` a link to a gitfile naming its own git dir:
    // git follows the link, finds it right, and leaves it be
    let h = ws.outside("h");
    let h_git_dir = ws.add_worktree(&app, &h, &["-b", "h"]);
    let gitfile = ws.outside("h.gitfile");
    std::fs::rename(h.join(".git"), &gitfile).unwrap();
    symlink(&gitfile, h.join(".git")).unwrap();
    ws.assert_head(&h, Some("h"));
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );

    // the link's gitfile naming another git dir: git would rewrite it
    let other = app.join(".git/worktrees/s");
    std::fs::write(&gitfile, format!("gitdir: {}\n", other.display())).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_git_dir)
        )]
    );

    // a `.git` file git can't parse: broken, git would rewrite it
    std::fs::remove_file(h.join(".git")).unwrap();
    std::fs::write(h.join(".git"), "not a gitfile\n").unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_git_dir)
        )]
    );
}

#[test]
fn two_swapped_worktrees_are_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wa = ws.dir("wa");
    let wa_git_dir = ws.add_worktree(&app, &wa, &["-b", "a"]);
    let wb = ws.dir("wb");
    let wb_git_dir = ws.add_worktree(&app, &wb, &["-b", "b"]);
    let tmp = ws.dir("tmp");
    std::fs::rename(&wa, &tmp).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    std::fs::rename(&tmp, &wb).unwrap();
    assert_eq!(points_at(&wa), "wb");
    assert_eq!(points_at(&wb), "wa");

    let origin = owned_origin("app");
    let swapped = |git_dir: &Path, with: &str| UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Swapped {
            git_dir: git_dir.to_str().unwrap().into(),
            with: with.into(),
        }),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [
            stray("wa", Some(&origin), true, swapped(&wa_git_dir, "wb")),
            stray("wb", Some(&origin), true, swapped(&wb_git_dir, "wa")),
        ]
    );

    // the advice holds: moved back, both are live
    std::fs::rename(&wa, &tmp).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    std::fs::rename(&tmp, &wb).unwrap();
    assert_eq!(ws.unregistered(), []);

    // a three-way rotation isn't a swap: wa's claimant has its own dir
    // claimed, but by another git dir than wa's
    let (ws, git_dir) =
        three_worktrees(&[("wa", "tmp"), ("wc", "wa"), ("wb", "wc"), ("tmp", "wb")]);
    assert_eq!(points_at(&ws.dir("wa")), "wc");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "wa",
                Some(&origin),
                true,
                moved_claimed("app", &git_dir("wa"))
            ),
            stray(
                "wb",
                Some(&origin),
                true,
                moved_claimed("app", &git_dir("wb"))
            ),
            stray(
                "wc",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wa"), &git_dir("wa"))
            ),
        ]
    );

    // nor is a three-step chain: wa's claimant is blocked by another dir
    // than its own
    let (ws, git_dir) = three_worktrees(&[("wa", "wa-old"), ("wb", "wa"), ("wc", "wb")]);
    assert_eq!(points_at(&ws.dir("wa-old")), "wa");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "wa",
                Some(&origin),
                true,
                moved_claimed("app", &git_dir("wa"))
            ),
            stray(
                "wa-old",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wb"), &git_dir("wb"))
            ),
            stray(
                "wb",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wa"), &git_dir("wa"))
            ),
        ]
    );
}

/// `app` with worktrees wa, wb, wc at the root, then the renames `moves`
/// made by hand, in order; returns the workspace and a worktree git dir by
/// id.
fn three_worktrees(moves: &[(&str, &str)]) -> (FixtureWorkspace, impl Fn(&str) -> PathBuf) {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    for id in ["wa", "wb", "wc"] {
        ws.add_worktree(&app, &ws.dir(id), &["-b", id]);
    }
    for (from, to) in moves {
        std::fs::rename(ws.dir(from), ws.dir(to)).unwrap();
    }
    let worktrees = app.join(".git/worktrees");
    (ws, move |id: &str| worktrees.join(id))
}

#[test]
fn a_repair_git_complains_through_is_offered_with_its_noise() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a path that's now a plain file
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    std::fs::write(&y, "a file\n").unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // git complains and exits 1, repairing it all the same
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&y).unwrap(), "a file\n");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_dangling_link_at_a_worktrees_path_is_noise() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a path that's now a dangling link: git looks at the
    // link itself, not its target, so it walks `y` and complains
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    let nowhere = ws.outside("nowhere");
    symlink(&nowhere, &y).unwrap();
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // git complains and exits 1, repairing it all the same
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
    assert_eq!(std::fs::read_link(&y).unwrap(), nowhere);
    assert!(!nowhere.exists());
    assert_eq!(ws.unregistered(), []);
}

/// Runs `f` on its own thread, failing (not hanging) the test when it
/// doesn't finish within a minute.
fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let done = rx.recv_timeout(std::time::Duration::from_secs(60));
    assert!(done.is_ok(), "blocked: not finished within a minute");
    done.unwrap()
}

/// Makes a FIFO at `path`.
fn mkfifo(ws: &FixtureWorkspace, path: &Path) {
    let out = ws.command("mkfifo", ws.base()).arg(path).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(std::fs::metadata(path).unwrap().file_type().is_fifo());
}

#[test]
fn a_git_that_is_a_fifo_is_reported_not_read() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let fifo = ws.dir("fifo");
    std::fs::create_dir(&fifo).unwrap();
    mkfifo(&ws, &fifo.join(".git"));
    // git fails fast on it
    ws.git_fails(&fifo, &["status"]);

    let found = within_a_minute(move || ws.unregistered());
    assert_eq!(
        found,
        [stray("fifo", None, false, UnregisteredKind::Worktree)]
    );
}

#[test]
fn a_repair_leaves_the_old_path_of_its_own_git_dir_alone() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // moved out; its old path now holds a repo of its own: git's walk would
    // complain about it if another git dir named it, but this one's is
    // repointed first
    let old = ws.outside("s");
    ws.add_worktree(&app, &old, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&old, &s_moved).unwrap();
    ws.git(
        ws.base(),
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "-q",
            old.to_str().unwrap(),
        ],
    );

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved("app")
        )]
    );
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_old_path_is_now_a_file_is_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let old = ws.outside("s");
    ws.add_worktree(&app, &old, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&old, &s_moved).unwrap();
    std::fs::write(&old, "a file\n").unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved("app")
        )]
    );
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "a file\n");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_blocking_path_shows_as_the_git_dir_writes_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names its worktree through a link: git's messages name
    // the linked path, and so does the report
    let real = ws.outside("real");
    std::fs::create_dir(&real).unwrap();
    let link = ws.outside("link");
    symlink(&real, &link).unwrap();
    let y = real.join("y");
    let y_git_dir = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_file(y.join(".git")).unwrap();
    let written = link.join("y");
    std::fs::write(
        y_git_dir.join("gitdir"),
        format!("{}\n", written.join(".git").display()),
    )
    .unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &written, &y_git_dir)
        )]
    );
}

#[test]
fn a_relative_gitdir_blocks_every_repair_in_its_repo() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `k` names its live worktree relatively: git 2.48+ resolves it
    // against the git dir, older gits against the cwd — where a repair
    // would write a `.git` into whatever dir that names
    let k = ws.outside("k");
    let k_git_dir = ws.add_worktree(&app, &k, &["-b", "k"]);
    std::fs::write(k_git_dir.join("gitdir"), "../../../../../k/.git\n").unwrap();
    assert_eq!(
        k_git_dir.join("../../../../../k").canonicalize().unwrap(),
        k
    );
    // an unrelated worktree, moved
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    let origin = owned_origin("app");

    let blocked = [stray(
        "s-moved",
        Some(&origin),
        true,
        moved_relative("app", &k_git_dir),
    )];
    assert_eq!(ws.unregistered(), blocked);
    // and whatever else stands in the way, the relative gitdir comes first:
    // `k`'s `.git` gone (a rewrite), then a dir (noise)
    std::fs::remove_file(k.join(".git")).unwrap();
    assert_eq!(ws.unregistered(), blocked);
    std::fs::create_dir(k.join(".git")).unwrap();
    assert_eq!(ws.unregistered(), blocked);
}

#[test]
fn a_noisy_path_shows_as_the_git_dir_writes_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names its worktree through a link, and its `.git` is a
    // dir: git complains about the linked path, and so does the report
    let real = ws.outside("real");
    std::fs::create_dir(&real).unwrap();
    let link = ws.outside("link");
    symlink(&real, &link).unwrap();
    let y = real.join("y");
    let y_git_dir = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_file(y.join(".git")).unwrap();
    std::fs::create_dir(y.join(".git")).unwrap();
    let written = link.join("y");
    std::fs::write(
        y_git_dir.join("gitdir"),
        format!("{}\n", written.join(".git").display()),
    )
    .unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &written)
        )]
    );
}

/// `git -C <app> worktree add -q <path> <args>` for a path that may not be
/// UTF-8; returns its git dir, `<common>/worktrees/<id>`.
fn add_worktree_at(ws: &FixtureWorkspace, app: &Path, path: &Path, args: &[&str]) -> PathBuf {
    let out = ws
        .command("git", app)
        .args([OsStr::new("worktree"), OsStr::new("add"), OsStr::new("-q")])
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let out = ws.git_output(path, &["rev-parse", "--absolute-git-dir"]);
    assert!(out.status.success(), "{out:?}");
    let git_dir = out.stdout.strip_suffix(b"\n").unwrap();
    PathBuf::from(OsStr::from_bytes(git_dir))
}

/// Swaps two dirs by hand.
fn swap_dirs(ws: &FixtureWorkspace, a: &Path, b: &Path) {
    let tmp = ws.dir("swap-tmp");
    std::fs::rename(a, &tmp).unwrap();
    std::fs::rename(b, a).unwrap();
    std::fs::rename(&tmp, b).unwrap();
}

/// `git worktree list --porcelain`'s raw bytes, paths as git lists them.
fn worktree_list(ws: &FixtureWorkspace, app: &Path) -> Vec<u8> {
    let out = ws
        .command("git", app)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    out.stdout
}

/// Whether git lists a worktree at exactly `path`, bytes and all.
fn lists_worktree(list: &[u8], path: &Path) -> bool {
    let line = [b"worktree ", path.as_os_str().as_bytes(), b"\n"].concat();
    list.windows(line.len()).any(|w| w == line)
}

/// Moved, but swapped by hand with `with`: `git_dir` names this dir.
fn swapped(git_dir: &Path, with: &str) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Swapped {
            git_dir: git_dir.to_string_lossy().into_owned(),
            with: with.into(),
        }),
        exit_noise: None,
    }
}

#[test]
fn a_swap_with_a_worktree_whose_path_is_not_utf8_is_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let a = ws.dir("a");
    let a_git_dir = add_worktree_at(&ws, &app, &a, &["-b", "a"]);
    let b = ws.root().join(OsStr::from_bytes(b"b\xff"));
    let b_git_dir = add_worktree_at(&ws, &app, &b, &["-b", "b"]);
    assert_eq!(b_git_dir.file_name(), Some(OsStr::from_bytes(b"b\xff")));
    swap_dirs(&ws, &a, &b);
    // git's view: each git dir still names its old path, each dir holds the
    // other's checkout
    let list = worktree_list(&ws, &app);
    assert!(lists_worktree(&list, &a) && lists_worktree(&list, &b));
    ws.assert_head(&a, Some("b"));
    ws.assert_head(&b, Some("a"));

    // no repair: either would hijack the other
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("a", Some(&origin), true, swapped(&a_git_dir, "b\u{fffd}")),
            stray("b\u{fffd}", Some(&origin), true, swapped(&b_git_dir, "a")),
        ]
    );
    // the advice holds: moved back, both are live
    swap_dirs(&ws, &a, &b);
    ws.assert_head(&a, Some("a"));
    ws.assert_head(&b, Some("b"));
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_swap_after_a_move_to_a_path_that_is_not_utf8_is_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let a = ws.dir("a");
    let a_git_dir = add_worktree_at(&ws, &app, &a, &["-b", "a"]);
    let b_git_dir = add_worktree_at(&ws, &app, &ws.dir("b"), &["-b", "b"]);
    // moved by git, so its git dir keeps its UTF-8 id and names the new path
    let b = ws.root().join(OsStr::from_bytes(b"b\xff"));
    let out = ws
        .command("git", &app)
        .args(["worktree", "move"])
        .arg(ws.dir("b"))
        .arg(&b)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(lists_worktree(&worktree_list(&ws, &app), &b));
    assert_eq!(ws.unregistered(), []);
    swap_dirs(&ws, &a, &b);
    ws.assert_head(&a, Some("b"));
    ws.assert_head(&b, Some("a"));

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("a", Some(&origin), true, swapped(&a_git_dir, "b\u{fffd}")),
            stray("b\u{fffd}", Some(&origin), true, swapped(&b_git_dir, "a")),
        ]
    );
    swap_dirs(&ws, &a, &b);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_gitdir_past_the_tools_limit_blocks_every_repair_in_its_repo() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `h`'s gitdir padded past the tool's limit: git reads it whole and
    // lists `h`, but the tool can't tell what a repair's walk does with it
    let h = ws.dir("h");
    let h_git_dir = ws.add_worktree(&app, &h, &["-b", "h"]);
    let mut padded = h.join(".git").as_os_str().as_bytes().to_vec();
    padded.resize(2 * 1024 * 1024, b'\n');
    std::fs::write(h_git_dir.join("gitdir"), &padded).unwrap();
    ws.worktree_record(&app, &h);

    let origin = owned_origin("app");
    let unreadable = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::UnreadableGitdir {
            git_dir: h_git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    };
    // and `h` itself, which the tool can't tell is live, fails closed too
    assert_eq!(
        ws.unregistered(),
        [
            stray("h", Some(&origin), true, shared_unnamed("app")),
            stray("s-moved", Some(&origin), true, unreadable),
        ]
    );
    // trimmed back, the repair is offered, and holds
    std::fs::write(
        h_git_dir.join("gitdir"),
        format!("{}\n", h.join(".git").display()),
    )
    .unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_path_is_not_utf8_gets_no_repair_command() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let b = ws.dir("b");
    ws.add_worktree(&app, &b, &["-b", "b"]);
    let odd = ws.root().join(OsStr::from_bytes(b"b\xff"));
    std::fs::rename(&b, &odd).unwrap();
    ws.assert_head(&odd, Some("b"));
    // the path shown lossily names no dir: git refuses to repair it
    let lossy = odd.to_string_lossy().into_owned();
    ws.git_fails(&app, &["worktree", "repair", &lossy]);

    let origin = owned_origin("app");
    let blocked = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::NonUtf8Path),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [stray("b\u{fffd}", Some(&origin), true, blocked)]
    );
    // the advice holds: renamed to a UTF-8 name, the repair is offered and
    // reconnects it
    let renamed = ws.dir("b-renamed");
    std::fs::rename(&odd, &renamed).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray("b-renamed", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", renamed.to_str().unwrap()]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_repair_that_would_rewrite_a_checkout_whose_path_is_not_utf8_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `q\xff`, its `.git` gone: git's repair walk would write one there
    let q = ws.root().join(OsStr::from_bytes(b"q\xff"));
    let q_git_dir = add_worktree_at(&ws, &app, &q, &["-b", "q"]);
    std::fs::remove_file(q.join(".git")).unwrap();
    assert!(lists_worktree(&worktree_list(&ws, &app), &q));

    let origin = owned_origin("app");
    let blocked = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Rewrites {
            path: q.to_string_lossy().into_owned(),
            git_dir: q_git_dir.to_string_lossy().into_owned(),
        }),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, blocked)]
    );
    // as git does, when the repair is run anyway
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(q.join(".git").is_file());
}
