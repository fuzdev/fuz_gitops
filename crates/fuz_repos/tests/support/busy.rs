//! Helpers shared by the `status_busy_*` tests: session builders, the reader,
//! and the fixture repos.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{ClaudeDir, FixtureWorkspace, LiveChild, path, write};
use fuz_repos::sessions::{
    LiveSessions, Session, SessionSource, SessionsSource, read_live_sessions,
};
use fuz_repos::state::{BranchHold, SyncAction, Verdict};

pub const fn push(commits: u32) -> SyncAction {
    SyncAction::Push { commits }
}

pub const fn held(action: SyncAction, by: BranchHold) -> Verdict {
    Verdict::Held { action, by }
}

/// A session no process backs, for the scoping alone: its start time is
/// never checked.
pub fn session(pid: u32, cwd: &Path, source: SessionSource) -> Session {
    Session::at(pid, 0, path(cwd), source)
}

/// A session of one of the test's own children, with its start time, as
/// the reader vouches for it.
pub fn live_session(child: &LiveChild, cwd: &Path, source: SessionSource) -> Session {
    Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        path(cwd),
        source,
    )
}

/// `live_session` for a child spawned where the test runs
/// (`LiveChild::spawn`): its process's cwd.
pub fn child_session(child: &LiveChild, cwd: &Path, source: SessionSource) -> Session {
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_ne!(here, cwd);
    Session {
        process_cwd: Some(path(&here)),
        ..live_session(child, cwd, source)
    }
}

/// Where the tool would look given `dirs`, with no `CLAUDE_PID`.
pub fn source(dirs: &[&ClaudeDir]) -> SessionsSource {
    SessionsSource {
        config_dirs: Ok(dirs.iter().map(|d| d.0.clone()).collect()),
        claude_pid: None,
        ancestors: BTreeMap::new(),
    }
}

/// Reads `claude` as the tool would, no caller excluded.
pub fn read(claude: &ClaudeDir) -> LiveSessions {
    read_live_sessions(&source(&[claude]))
}

/// Reads `claude` with `CLAUDE_PID` set to `caller`'s pid, and `caller`
/// among this process's ancestors when `ancestor`.
pub fn read_as(claude: &ClaudeDir, caller: &LiveChild, ancestor: bool) -> LiveSessions {
    let mut source = source(&[claude]);
    source.claude_pid = Some(caller.pid());
    if ancestor {
        let start = caller.proc_start().parse().unwrap();
        source.ancestors.insert(caller.pid(), start);
    }
    read_live_sessions(&source)
}

/// A fresh config dir under the fixture's tempdir.
pub fn claude_dir(ws: &FixtureWorkspace, name: &str) -> ClaudeDir {
    ClaudeDir::new(ws.outside(name))
}

/// Sorted as the reader lists sessions: by pid, then cwd.
pub fn by_pid(mut sessions: Vec<Session>) -> LiveSessions {
    sessions.sort_by(|a, b| (a.pid, &a.cwd).cmp(&(b.pid, &b.cwd)));
    LiveSessions::Known(sessions)
}

/// Commits once on `branch` in `repo` and pushes it with an upstream, then
/// commits once more locally: ahead 1.
pub fn ahead_branch(ws: &FixtureWorkspace, repo: &Path, branch: &str) {
    ws.git(repo, &["push", "-q", "origin", &format!("main:{branch}")]);
    ws.git(
        repo,
        &[
            "branch",
            "-q",
            "--track",
            branch,
            &format!("origin/{branch}"),
        ],
    );
    ws.git(repo, &["checkout", "-q", branch]);
    ws.commit(repo, &format!("local-{branch}"));
    ws.git(repo, &["checkout", "-q", "main"]);
    ws.assert_track(repo, branch, "[ahead 1]");
}

/// `app` with Claude Code's worktrees dir ignored, as a user's global
/// excludes would, so a worktree nested in it leaves the primary clean.
pub fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    write(&app, ".git/info/exclude", ".claude/\n");
    assert_ignores_claude_worktrees(ws, &app);
    app
}

/// Asserts git ignores a worktree nested where Claude Code puts one under
/// `repo` (`.claude/worktrees/<name>`), and `repo` is clean: one added
/// there later leaves it clean.
pub fn assert_ignores_claude_worktrees(ws: &FixtureWorkspace, repo: &Path) {
    ws.git(repo, &["check-ignore", "-q", ".claude/worktrees/x/a.txt"]);
    ws.assert_clean(repo);
}

/// `app` with `main` ahead in the primary and `feat` ahead in a linked
/// worktree at `app-feat`; returns the primary, the worktree, and its own
/// git dir.
pub fn app_with_a_feat_worktree(ws: &mut FixtureWorkspace) -> (PathBuf, PathBuf, PathBuf) {
    let app = app(ws);
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ahead_branch(ws, &app, "feat");
    let wt = ws.dir("app-feat");
    let git_dir = ws.add_worktree(&app, &wt, &["feat"]);
    (app, wt, git_dir)
}

/// Moves `app`'s worktree `wt` to `to` with a plain `mv`, and checks git
/// still works there, on its branch, committing to it: `feat` ends ahead 2.
pub fn move_by_hand(ws: &FixtureWorkspace, app: &Path, wt: &Path, to: &Path) {
    std::fs::rename(wt, to).unwrap();
    ws.assert_head(to, Some("feat"));
    ws.commit(to, "moved");
    ws.assert_track(app, "feat", "[ahead 2]");
    // git doesn't repair the move
    let record = ws.worktree_record(app, wt);
    assert!(
        record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );
}

/// git's own limit on a `.git` file (`read_gitfile_gently`).
pub const MAX_GITFILE_BYTES: usize = 1024 * 1024;
