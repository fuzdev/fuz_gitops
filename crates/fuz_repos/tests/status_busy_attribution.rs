//! Attributing a session to the worktree or checkout its cwd reaches through
//! moved, copied, and hand-written git files.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;

use fuz_repos::report::Sessions;
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{BranchHold, Prune, UnprobedWhy, Verdict};
use support::busy::{
    MAX_GITFILE_BYTES, app, app_with_a_feat_worktree, held, move_by_hand, push, session,
};
use support::{FixtureWorkspace, branch, find_entry, path};

/// A session at `cwd` is attributed to `app`'s worktree at `checkout`, by
/// the `.git` it walks up to, and to nothing else: `feat`'s push is held as
/// busy while the primary's acts.
fn assert_feat_busy(ws: &FixtureWorkspace, cwd: &Path, checkout: &Path, commits: u32) {
    let s = session(9, cwd, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    let busy: Vec<(&str, &[Session])> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.busy.as_slice()))
        .chain(
            e.unprobed_worktrees
                .iter()
                .map(|u| (u.worktree.path.as_str(), u.busy.as_slice())),
        )
        .filter(|(_, busy)| !busy.is_empty())
        .collect();
    assert_eq!(busy, [(path(checkout).as_str(), std::slice::from_ref(&s))]);
    assert_eq!(
        branch(e, "feat").verdict,
        held(push(commits), BranchHold::Busy)
    );
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
}

#[test]
fn a_session_in_a_worktree_moved_by_hand_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
    // out of the workspace root, where the scan wouldn't see it
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    std::fs::create_dir(moved.join("src")).unwrap();
    // the probe knows it only by its old path, gone
    let run = ws.status_live(&LiveSessions::Known(vec![]));
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    assert_eq!(e.unprobed_worktrees[0].worktree.why, UnprobedWhy::Prunable);
    assert_eq!(e.unprobed_worktrees[0].prune, Some(Prune::Safe));
    // with nobody in it, its push acts
    assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(2) });

    // a session in it, or deeper, finds its `.git`, which names the
    // worktree's own git dir
    assert_feat_busy(&ws, &moved, &wt, 2);
    assert_feat_busy(&ws, &moved.join("src"), &wt, 2);
    // a repo nested in it, or a `.git` naming nothing, is passed over: git
    // itself would find the nested repo, but the session sits in the
    // worktree's files all the same
    let vendor = moved.join("vendor");
    ws.git(&moved, &["init", "-q", "vendor"]);
    assert!(vendor.join(".git").is_dir());
    assert_feat_busy(&ws, &vendor, &wt, 2);
    let dangling = moved.join("dangling");
    support::write(&dangling, ".git", "gitdir: ../nowhere\n");
    assert_feat_busy(&ws, &dangling, &wt, 2);
}

#[test]
fn a_worktree_moved_into_another_checkout_is_busy_with_it() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
    let lib = ws.owned_repo("lib", &[]);
    ws.commit(&lib, "local");
    ws.assert_track(&lib, "main", "[ahead 1]");
    // into `lib`'s tree: the path puts a session there in `lib`
    let moved = lib.join("app-feat");
    move_by_hand(&ws, &app, &wt, &moved);

    let s = session(9, &moved, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    // and its `.git` in `app`'s worktree: both hold, pushes included
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.unprobed_worktrees[0].worktree.path, path(&wt));
    assert_eq!(e.unprobed_worktrees[0].busy, std::slice::from_ref(&s));
    assert!(e.checkouts.iter().all(|c| c.busy.is_empty()));
    assert_eq!(branch(e, "feat").verdict, held(push(2), BranchHold::Busy));
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    let lib = find_entry(&run.entries, "lib");
    assert_eq!(lib.checkouts[0].busy, [s]);
    assert_eq!(branch(lib, "main").verdict, held(push(1), BranchHold::Busy));
}

/// Copies `from` to `to` as `cp -a` does, symlinks and modes kept.
fn copy_tree(from: &Path, to: &Path) {
    let out = std::process::Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_session_in_a_copy_of_a_worktree_is_attributed_to_it() {
    for inside in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
        // a copy's `.git` still names the worktree's own git dir, so
        // committing there moves its branch
        let copy = if inside {
            ws.dir("app-copy")
        } else {
            ws.outside("app-copy")
        };
        copy_tree(&wt, &copy);
        ws.assert_head(&copy, Some("feat"));
        ws.commit(&copy, "copied");
        ws.assert_track(&app, "feat", "[ahead 2]");
        // the worktree itself stays where git lists it
        ws.assert_head(&wt, Some("feat"));

        assert_feat_busy(&ws, &copy, &wt, 2);
    }
}

#[test]
fn a_session_in_a_copy_of_a_separate_git_dir_primary_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app-git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    assert!(app.join(".git").is_file());
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    let copy = ws.outside("app-copy");
    copy_tree(&app, &copy);
    ws.assert_head(&copy, Some("main"));

    let s = session(9, &copy, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
}

#[test]
fn a_gone_worktree_with_nobody_in_it_holds_only_what_a_push_does_not_touch() {
    for locked in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, _) = app_with_a_feat_worktree(&mut ws);
        if locked {
            // as on media that's been unmounted
            ws.git(&app, &["worktree", "lock", wt.to_str().unwrap()]);
        }
        std::fs::remove_dir_all(&wt).unwrap();
        let why = if locked {
            UnprobedWhy::Missing
        } else {
            UnprobedWhy::Prunable
        };
        // a session elsewhere changes nothing
        let at_root = session(9, &ws.root(), SessionSource::SessionFile);
        let run = ws.status_live(&LiveSessions::Known(vec![at_root.clone()]));
        assert_eq!(
            run.sessions,
            Sessions::Available {
                unscoped: vec![at_root]
            }
        );
        let e = find_entry(&run.entries, "app");
        assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
        assert_eq!(e.unprobed_worktrees[0].worktree.why, why);
        assert_eq!(
            branch(e, "feat").verdict,
            Verdict::Act { action: push(1) },
            "{why:?}"
        );
        assert_eq!(branch(e, "main").verdict, Verdict::Act { action: push(1) });
    }
}

#[test]
fn a_session_in_a_worktree_whose_gitdir_names_no_path_is_attributed_to_it() {
    for lost in ["missing", "empty", "unreadable"] {
        let mut ws = FixtureWorkspace::new();
        let (app, wt, git_dir) = app_with_a_feat_worktree(&mut ws);
        let gitdir = git_dir.join("gitdir");
        let _unseal = match lost {
            "missing" => {
                std::fs::remove_file(&gitdir).unwrap();
                None
            }
            "empty" => {
                std::fs::write(&gitdir, "").unwrap();
                None
            }
            _ => {
                let Some(unseal) = support::seal(&gitdir, 0o000) else {
                    return;
                };
                Some(unseal)
            }
        };
        // git drops it from its list, but it still works there
        let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
        assert!(!list.contains(&path(&wt)), "{lost}: {list}");
        ws.assert_head(&wt, Some("feat"));

        // its only path is its own git dir, which the `.git` a session in
        // it walks up to names
        assert_feat_busy(&ws, &wt, &git_dir, 1);
        let run = ws.status_live(&LiveSessions::Known(vec![]));
        let e = find_entry(&run.entries, "app");
        assert_eq!(
            e.unprobed_worktrees[0].worktree.path,
            path(&git_dir),
            "{lost}"
        );
        assert!(e.needs_human.is_empty(), "{lost}: {:?}", e.needs_human);
        assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(1) });
    }
}

/// Writes `dir/.git` with raw `content`, creating `dir`.
fn write_dot_git(dir: &Path, content: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".git"), content).unwrap();
}

/// A gitfile naming `git_dir`, padded with line breaks to `len` bytes.
fn padded_gitfile(git_dir: &Path, len: usize) -> Vec<u8> {
    let mut bytes = format!("gitdir: {}", git_dir.display()).into_bytes();
    bytes.resize(len, b'\n');
    bytes
}

/// Whether git run in `dir` stops with an error, finding no repo it can use.
fn git_stops_at(ws: &FixtureWorkspace, dir: &Path) -> bool {
    !ws.git_output(dir, &["rev-parse", "--absolute-git-dir"])
        .status
        .success()
}

#[test]
fn a_dot_git_git_cannot_use_is_passed_over() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, git_dir) = app_with_a_feat_worktree(&mut ws);
    // out of the workspace root, so only the walk finds it
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    // under the moved worktree, and under no checkout at all
    let loose = ws.outside("loose");
    for parent in [&moved, &loose] {
        // each `.git` git stops at with an error: garbage, over its size
        // limit, naming nothing, or naming a path through a symlink loop
        let garbage = parent.join("garbage");
        write_dot_git(&garbage, b"not a gitfile\n");
        let oversize = parent.join("oversize");
        write_dot_git(&oversize, &padded_gitfile(&git_dir, MAX_GITFILE_BYTES + 1));
        let nowhere = parent.join("nowhere");
        write_dot_git(&nowhere, b"gitdir: /nonexistent-git-dir\n");
        let looped = parent.join("looped");
        std::fs::create_dir(&looped).unwrap();
        std::os::unix::fs::symlink(".git", looped.join(".git")).unwrap();
        let through_loop = parent.join("through-loop");
        write_dot_git(&through_loop, b"gitdir: ../looped/.git/x\n");
        for dir in [&garbage, &oversize, &nowhere, &through_loop] {
            assert!(git_stops_at(&ws, dir), "{}", dir.display());
        }
        // and one it passes over: the loop itself
        let dirs = [&garbage, &oversize, &nowhere, &looped, &through_loop];
        if parent == &moved {
            assert_eq!(
                ws.git(&looped, &["rev-parse", "--absolute-git-dir"]),
                path(&git_dir)
            );
            // the moved worktree's `.git` above them is the walk's
            for dir in dirs {
                assert_feat_busy(&ws, dir, &wt, 2);
            }
        } else {
            for dir in dirs {
                assert!(git_stops_at(&ws, dir), "{}", dir.display());
                let s = session(9, dir, SessionSource::SessionFile);
                let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
                assert_eq!(run.sessions, Sessions::Available { unscoped: vec![s] });
                let e = find_entry(&run.entries, "app");
                assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(2) });
            }
        }
    }
    // one that can't be read, or a `.git` dir that can't: git stops at the
    // gitfile and passes over the dir. Last: sealing binds only a non-root
    // user
    let sealed = moved.join("sealed");
    write_dot_git(
        &sealed,
        format!("gitdir: {}\n", git_dir.display()).as_bytes(),
    );
    let sealed_dir = moved.join("sealed-dir");
    std::fs::create_dir_all(sealed_dir.join(".git")).unwrap();
    let Some(_unseal) = support::seal(&sealed.join(".git"), 0o000) else {
        return;
    };
    let Some(_unseal_dir) = support::seal(&sealed_dir.join(".git"), 0o000) else {
        return;
    };
    assert!(git_stops_at(&ws, &sealed));
    assert_eq!(
        ws.git(&sealed_dir, &["rev-parse", "--absolute-git-dir"]),
        path(&git_dir)
    );
    assert_feat_busy(&ws, &sealed, &wt, 2);
    assert_feat_busy(&ws, &sealed_dir, &wt, 2);
}

/// Rewrites the `.git` of `app`'s worktree `wt` as `content`, moves it by
/// hand to a dir outside the workspace root — committing there through it,
/// so git itself reads it as a gitfile — and checks a session there is
/// attributed to the worktree.
fn assert_gitfile_followed(content: &dyn Fn(&Path) -> Vec<u8>) {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, git_dir) = app_with_a_feat_worktree(&mut ws);
    std::fs::write(wt.join(".git"), content(&git_dir)).unwrap();
    let moved = ws.outside("elsewhere");
    move_by_hand(&ws, &app, &wt, &moved);
    assert_eq!(
        ws.git(&moved, &["rev-parse", "--absolute-git-dir"]),
        path(&git_dir)
    );
    assert_feat_busy(&ws, &moved, &wt, 2);
}

#[test]
fn a_gitfile_is_followed_where_git_follows_it() {
    // a C string: what follows a NUL is ignored
    assert_gitfile_followed(&|git_dir| {
        let mut bytes = format!("gitdir: {}", git_dir.display()).into_bytes();
        bytes.extend(b"\0junk\n");
        bytes
    });
    // line breaks trimmed however many, up to git's size limit
    assert_gitfile_followed(&|git_dir| padded_gitfile(git_dir, 100_000));
    assert_gitfile_followed(&|git_dir| padded_gitfile(git_dir, MAX_GITFILE_BYTES));
}

#[test]
fn a_gitfile_naming_a_path_that_is_not_utf8_is_followed() {
    use std::os::unix::ffi::OsStrExt as _;
    let probe = tempfile::tempdir().unwrap();
    let name = std::ffi::OsStr::from_bytes(b"git-\xff");
    if std::fs::create_dir(probe.path().join(name)).is_err() {
        eprintln!("skipped: the filesystem refuses a name that isn't UTF-8");
        return;
    }
    assert_gitfile_followed(&|git_dir| {
        // the common dir by a symlink whose name isn't UTF-8
        let common = git_dir.parent().unwrap().parent().unwrap();
        let link = common.parent().unwrap().parent().unwrap().join(name);
        std::os::unix::fs::symlink(common, &link).unwrap();
        let mut bytes = b"gitdir: ".to_vec();
        bytes.extend(link.as_os_str().as_bytes());
        bytes.extend(b"/worktrees/");
        bytes.extend(git_dir.file_name().unwrap().as_bytes());
        bytes.push(b'\n');
        bytes
    });
}

#[test]
fn a_dot_git_symlinked_to_a_checkouts_git_dir_is_attributed_to_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    ws.commit(&app, "local");
    // outside every checkout, its `.git` the primary's git dir
    let lnk = ws.outside("lnk");
    std::fs::create_dir(&lnk).unwrap();
    std::os::unix::fs::symlink(app.join(".git"), lnk.join(".git")).unwrap();
    ws.git(&lnk, &["commit", "-q", "--allow-empty", "-m", "via-link"]);
    ws.assert_track(&app, "main", "[ahead 2]");

    let s = session(9, &lnk, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(2), BranchHold::Busy));
}

#[test]
fn a_session_inside_a_git_dir_works_in_its_checkout() {
    let mut ws = FixtureWorkspace::new();
    let (app, wt, git_dir) = app_with_a_feat_worktree(&mut ws);
    // git takes the git dir for the repo, and moves the branch its HEAD is
    // on from there
    assert_eq!(ws.git(&git_dir, &["rev-parse", "--git-dir"]), ".");
    let commit = ws.git(
        &git_dir,
        &["commit-tree", "HEAD:", "-p", "HEAD", "-m", "inside"],
    );
    ws.git(&git_dir, &["update-ref", "HEAD", &commit]);
    ws.assert_track(&app, "feat", "[ahead 2]");

    let s = session(9, &git_dir, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    // the worktree by its git dir, the primary by the path it sits in
    let busy: Vec<(&str, usize)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.busy.len()))
        .collect();
    assert_eq!(busy, [(path(&app).as_str(), 1), (path(&wt).as_str(), 1)]);
    assert_eq!(branch(e, "feat").verdict, held(push(2), BranchHold::Busy));
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
}

#[test]
fn a_session_inside_a_separate_git_dir_works_in_its_checkout() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app-git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    // below it, outside every checkout
    let refs = git_dir.join("refs");
    assert_eq!(
        ws.git(&refs, &["rev-parse", "--absolute-git-dir"]),
        path(&git_dir)
    );

    let s = session(9, &refs, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert_eq!(e.checkouts[0].busy, [s]);
    assert_eq!(branch(e, "main").verdict, held(push(1), BranchHold::Busy));
}
