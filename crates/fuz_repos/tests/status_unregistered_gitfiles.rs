//! Gitfiles, commondirs, and gitdir lines of unregistered dirs, read as git
//! reads them.

mod support;

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use fuz_repos::report::{RepairBlock, UnregisteredKind};
use support::unregistered::{app, copy_dir, moved, moved_by_hand, moved_rewrites, shared, stray};
use support::{FixtureWorkspace, owned_origin};

/// A gitfile naming `git_dir`, with `head` before it and `tail` after.
fn gitfile(head: &[u8], git_dir: &Path, tail: &[u8]) -> Vec<u8> {
    [head, b"gitdir: ", git_dir.as_os_str().as_bytes(), tail].concat()
}

#[test]
fn a_strays_gitfile_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git follows a path cut at a NUL
    let (nul, nul_git_dir) = moved_by_hand(&ws, &app, "nul");
    std::fs::write(nul.join(".git"), gitfile(b"", &nul_git_dir, b"\0junk\n")).unwrap();
    ws.assert_head(&nul, Some("nul"));
    // and refuses `gitdir: ` on a second line
    let (second, second_git_dir) = moved_by_hand(&ws, &app, "second");
    std::fs::write(second.join(".git"), gitfile(b"x\n", &second_git_dir, b"\n")).unwrap();
    ws.git_fails(&second, &["status"]);

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("nul-moved", Some(&origin), true, moved("app")),
            stray("second-moved", None, false, UnregisteredKind::Worktree),
        ]
    );
    // the advice holds: repair reconnects the one git follows
    ws.git(&app, &["worktree", "repair", nul.to_str().unwrap()]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "second-moved",
            None,
            false,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_moved_worktrees_commondir_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git follows a `commondir` cut at a NUL
    let (nul, nul_git_dir) = moved_by_hand(&ws, &app, "nul");
    std::fs::write(nul_git_dir.join("commondir"), b"../..\0junk\n").unwrap();
    ws.assert_head(&nul, Some("nul"));
    // and keeps a trailing space, naming no common dir
    let (spaced, spaced_git_dir) = moved_by_hand(&ws, &app, "spaced");
    std::fs::write(spaced_git_dir.join("commondir"), b"../.. \n").unwrap();
    ws.git_fails(&spaced, &["status"]);

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("nul-moved", Some(&origin), true, moved("app")),
            stray("spaced-moved", None, false, UnregisteredKind::Worktree),
        ]
    );
    // the advice holds: repaired, it's a live worktree of `app`
    ws.git(&app, &["worktree", "repair", nul.to_str().unwrap()]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "spaced-moved",
            None,
            false,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_hazard_gitfile_is_parsed_as_git_parses_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `h` elsewhere, its `.git` naming its own git dir past a NUL: git reads
    // it right, and a repair leaves it be
    let h = ws.outside("h");
    let h_git_dir = ws.add_worktree(&app, &h, &["-b", "h"]);
    let past_nul = gitfile(b"", &h_git_dir, b"\0junk\n");
    std::fs::write(h.join(".git"), &past_nul).unwrap();
    ws.assert_head(&h, Some("h"));
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(std::fs::read(h.join(".git")).unwrap(), past_nul);
    assert_eq!(ws.unregistered(), []);

    // on a second line: git can't parse it, so a repair would rewrite it
    let s_again = ws.dir("s-again");
    std::fs::rename(&s_moved, &s_again).unwrap();
    std::fs::write(h.join(".git"), gitfile(b"x\n", &h_git_dir, b"\n")).unwrap();
    ws.git_fails(&h, &["status"]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-again",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_git_dir)
        )]
    );
    // as it does, when run anyway
    ws.git_output(&app, &["worktree", "repair", s_again.to_str().unwrap()]);
    let rewritten = std::fs::read_to_string(h.join(".git")).unwrap();
    assert!(rewritten.starts_with("gitdir: "), "{rewritten:?}");
    ws.assert_head(&h, Some("h"));
}

#[test]
fn a_nul_in_a_worktree_git_dirs_gitdir_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let w = ws.dir("w");
    let git_dir = ws.add_worktree(&app, &w, &["-b", "w"]);
    // git strips a trailing `/.git` from the whole file, then cuts at the
    // NUL: it lists the worktree at `w/.git`, and `w` isn't live
    let gitdir = [w.join(".git").as_os_str().as_bytes(), b"\0junk\n"].concat();
    std::fs::write(git_dir.join("gitdir"), &gitdir).unwrap();
    ws.worktree_record(&app, &w.join(".git"));
    // yet a repair of `w` compares what's before the NUL with `w/.git`, sees
    // nothing to fix, and fails on the walk
    ws.git_fails(&app, &["worktree", "repair", w.to_str().unwrap()]);
    assert_eq!(std::fs::read(git_dir.join("gitdir")).unwrap(), gitdir);

    let origin = owned_origin("app");
    let nul = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::NulInGitdir {
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    };
    assert_eq!(ws.unregistered(), [stray("w", Some(&origin), true, nul)]);
    // the advice holds: `w/.git` written into its gitdir by hand, it's live
    std::fs::write(
        git_dir.join("gitdir"),
        format!("{}\n", w.join(".git").display()),
    )
    .unwrap();
    ws.worktree_record(&app, &w);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_copy_of_a_worktree_whose_gitfile_git_cuts_at_a_nul_shares_its_git_dir() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("wt");
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "wt"]);
    let copy = ws.dir("wt-copy");
    copy_dir(&ws, &wt, &copy);
    // the live one's `.git` names its git dir past a NUL: git follows it
    std::fs::write(wt.join(".git"), gitfile(b"", &git_dir, b"\0junk\n")).unwrap();
    ws.assert_head(&wt, Some("wt"));
    assert_eq!(
        ws.git(&wt, &["rev-parse", "--absolute-git-dir"]),
        git_dir.to_str().unwrap()
    );
    ws.worktree_record(&app, &wt);

    // so the copy shares it, and a repair there would take it from `wt`
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("wt-copy", Some(&origin), true, shared("app", &wt))]
    );
}
