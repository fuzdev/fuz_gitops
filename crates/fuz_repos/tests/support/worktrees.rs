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

/// Whether this git makes reftable repos (git 2.45 on), by making one
/// outside the workspace; says so on stderr when it doesn't, for a test to
/// skip on.
pub fn makes_reftable_repos(ws: &FixtureWorkspace) -> bool {
    let probe = ws.outside("reftable-probe");
    let made = ws.git_output(
        ws.base(),
        &[
            "init",
            "-q",
            "--ref-format=reftable",
            probe.to_str().unwrap(),
        ],
    );
    if !made.status.success() {
        eprintln!("skipped: this git makes no reftable repos");
    }
    made.status.success()
}

/// An owned repo cloned with its refs in the reftable format, a tracked
/// file in it, clean on `main`: its remote, its entry, and its clone at
/// `<root>/<name>`.
pub fn reftable_repo(ws: &mut FixtureWorkspace, name: &str) -> PathBuf {
    ws.remote(name, &[("tracked.txt", "one\n")]);
    ws.declare_repo(name, name, "");
    let repo = ws.clone_owned(name, name, &["--ref-format=reftable"]);
    assert_eq!(
        ws.git(&repo, &["rev-parse", "--show-ref-format"]),
        "reftable"
    );
    ws.assert_clean(&repo);
    repo
}
