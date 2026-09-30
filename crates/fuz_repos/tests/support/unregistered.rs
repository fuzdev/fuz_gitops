//! Helpers shared by the `status_unregistered_*` tests: expected scan kinds and
//! the fixture repos they read.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::FixtureWorkspace;
use fuz_repos::report::{RepairBlock, UnregisteredClone, UnregisteredKind};

pub fn stray(
    dir: &str,
    origin: Option<&str>,
    owned: bool,
    kind: UnregisteredKind,
) -> UnregisteredClone {
    UnregisteredClone {
        dir: dir.into(),
        origin: origin.map(str::to_owned),
        owned,
        kind,
    }
}

pub fn moved(entry: &str) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: None,
        exit_noise: None,
    }
}

/// Moved, but a repair would also rewrite `path`, which the worktree git
/// dir `git_dir` names.
pub fn moved_rewrites(entry: &str, path: &Path, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::Rewrites {
            path: path.to_str().unwrap().into(),
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

/// Moved, but a worktree git dir of the repo, `git_dir`, names its worktree
/// relatively, so no repair is certain.
pub fn moved_relative(entry: &str, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::RelativeGitdir {
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

/// `app`, an owned registered repo, clean on `main`.
pub fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    ws.assert_clean(&app);
    app
}

/// `cp -r from to`: a copy that keeps every file, `.git` included.
pub fn copy_dir(ws: &FixtureWorkspace, from: &Path, to: &Path) {
    let out = ws
        .command("cp", ws.base())
        .args([OsStr::new("-r"), from.as_os_str(), to.as_os_str()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The file a linked worktree's git dir names it by, as written.
pub fn gitdir_file(git_dir: &Path) -> String {
    std::fs::read_to_string(git_dir.join("gitdir"))
        .unwrap()
        .trim_end()
        .to_owned()
}

pub fn shared(entry: &str, with: &Path) -> UnregisteredKind {
    UnregisteredKind::SharedGitDir {
        entry: entry.into(),
        with: Some(with.to_str().unwrap().into()),
    }
}

pub fn shared_unnamed(entry: &str) -> UnregisteredKind {
    UnregisteredKind::SharedGitDir {
        entry: entry.into(),
        with: None,
    }
}

/// A worktree of `app` added at `<root>/<name>`, then moved by hand to
/// `<root>/<name>-moved`; returns the new path and its git dir.
pub fn moved_by_hand(ws: &FixtureWorkspace, app: &Path, name: &str) -> (PathBuf, PathBuf) {
    let wt = ws.dir(name);
    let git_dir = ws.add_worktree(app, &wt, &["-b", name]);
    let moved_to = ws.dir(&format!("{name}-moved"));
    std::fs::rename(&wt, &moved_to).unwrap();
    (moved_to, git_dir)
}

/// Where a checkout's `.git` file points, by the git dir's name.
pub fn points_at(checkout: &Path) -> String {
    let target = std::fs::read_to_string(checkout.join(".git")).unwrap();
    Path::new(target.trim_end())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}
