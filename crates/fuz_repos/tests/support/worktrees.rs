//! Helpers shared by the `status_worktrees_*` tests: the fixture repo and
//! the extra branches the worktrees hang off.

use std::path::{Path, PathBuf};

use super::FixtureWorkspace;
use fuz_repos::state::GitDirHolds;

/// A gone worktree's git dir holding nothing that isn't elsewhere.
pub const NOTHING_HELD: GitDirHolds = GitDirHolds {
    submodules: false,
    worktree_refs: false,
    staged: Some(false),
};

/// `app` with a tracked file, clean on `main`.
pub fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[("tracked.txt", "one\n")]);
    ws.assert_clean(&app);
    app
}

/// Creates `name` in `app` tracking `origin/<name>` and one commit behind it,
/// not checked out anywhere.
pub fn behind_branch(ws: &FixtureWorkspace, app: &Path, name: &str) {
    ws.upstream_commit("app", name);
    ws.git(app, &["fetch", "-q", "origin"]);
    ws.git(
        app,
        &["branch", "-q", "--track", name, &format!("origin/{name}")],
    );
    ws.upstream_commit("app", name);
    ws.git(app, &["fetch", "-q", "origin"]);
    ws.assert_track(app, name, "[behind 1]");
}

/// Creates `name` in `app` from `main` and pushes it with an upstream,
/// leaving it checked out nowhere.
pub fn pushed_branch(ws: &FixtureWorkspace, app: &Path, name: &str) {
    ws.git(app, &["branch", "-q", name, "main"]);
    ws.git(app, &["push", "-q", "-u", "origin", name]);
    ws.assert_track(app, name, "");
    ws.assert_upstream(app, name, &format!("refs/remotes/origin/{name}"));
}
