//! Worktrees Claude Code locked: busy with the live session the lock names.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};

use fuz_repos::busy::Sessions;
use fuz_repos::sessions::{LiveSessions, Session, SessionSource};
use fuz_repos::state::{HeldBy, UnprobedWhy, Verdict};
use support::busy::{ahead_branch, app, child_session, claude_dir, held, push, read, read_as};
use support::{FixtureWorkspace, LiveChild, branch, dead_pid, find_entry, path};

/// `app`, and `lib` with `feat` ahead 1 checked out in an agent worktree
/// where Claude Code puts one, `lib/.claude/worktrees/agent-a1`; returns
/// `app`, `lib`, and the worktree.
fn lib_with_an_agent_worktree(ws: &mut FixtureWorkspace) -> (PathBuf, PathBuf, PathBuf) {
    let app = app(ws);
    let lib = ws.owned_repo("lib", &[]);
    support::write(&lib, ".git/info/exclude", ".claude/\n");
    ahead_branch(ws, &lib, "feat");
    let wt = lib.join(".claude/worktrees/agent-a1");
    ws.add_worktree(&lib, &wt, &["feat"]);
    (app, lib, wt)
}

/// Locks `repo`'s worktree `wt` with `reason`, as `git worktree lock`
/// writes it, in place of any lock it had.
fn relock(ws: &FixtureWorkspace, repo: &Path, wt: &Path, reason: &str) {
    let wt = wt.to_str().unwrap();
    // fails when it isn't locked, which is as good
    let _ = ws.git_output(repo, &["worktree", "unlock", wt]);
    ws.git(repo, &["worktree", "lock", "--reason", reason, wt]);
}

/// Under `live`, `lib`'s checkout at `wt` is busy with `holder` alone and
/// `feat`'s push held — or, without a holder, nothing in `lib` is busy and
/// the push acts.
fn assert_lib_feat(
    ws: &FixtureWorkspace,
    live: &LiveSessions,
    wt: &Path,
    holder: Option<&Session>,
) {
    let run = ws.status_live(live);
    let e = find_entry(&run.entries, "lib");
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
    if let Some(s) = holder {
        assert_eq!(busy, [(path(wt).as_str(), std::slice::from_ref(s))]);
        assert_eq!(branch(e, "feat").verdict, held(push(1), HeldBy::Busy));
    } else {
        assert_eq!(busy, []);
        assert_eq!(branch(e, "feat").verdict, Verdict::Act { action: push(1) });
    }
}

#[test]
fn a_worktree_claude_code_locked_is_busy_with_the_live_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    let (app, lib, wt) = lib_with_an_agent_worktree(&mut ws);
    let claude = claude_dir(&ws, "claude");
    // launched in `app`, its agent worktree in `lib`, which it `cd`'d into
    // in the Bash tool: no place of it is in `lib`
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    claude.session(pid, &start, &app);
    let s = child_session(&agent, &app, SessionSource::SessionFile);
    let live = read(&claude);
    assert_eq!(live, LiveSessions::Known(vec![s.clone()]));
    // unlocked: nothing places it in `lib`
    assert_lib_feat(&ws, &live, &wt, None);

    // as Claude Code writes it, with its start time or without, for a
    // subagent's worktree or one a session entered
    for reason in [
        format!("claude agent agent-a1 (pid {pid} start {start})"),
        format!("claude agent agent-a1 (pid {pid})"),
        format!("claude session feat (pid {pid} start {start})"),
        format!("claude agent a (pid 1) b (pid {pid} start {start})"),
    ] {
        relock(&ws, &lib, &wt, &reason);
        assert_lib_feat(&ws, &live, &wt, Some(&s));
    }

    let dead = dead_pid();
    let reused = (start.parse::<u64>().unwrap() + 1).to_string();
    // a live process with no session: whatever it is, the reader never
    // vouched for it
    let unrecorded = LiveChild::spawn();
    let (other, other_start) = (unrecorded.pid(), unrecorded.proc_start());
    for reason in [
        // its process exited
        format!("claude agent agent-a1 (pid {dead} start {start})"),
        format!("claude agent agent-a1 (pid {dead})"),
        // left by a process whose pid is the session's now
        format!("claude agent agent-a1 (pid {pid} start {reused})"),
        format!("claude agent agent-a1 (pid {pid} start 0{start})"),
        format!("claude agent agent-a1 (pid {other} start {other_start})"),
        // not Claude Code's
        format!("claude worker agent-a1 (pid {pid} start {start})"),
        format!("Claude agent agent-a1 (pid {pid} start {start})"),
        format!("agent agent-a1 (pid {pid} start {start})"),
        format!("claude agent  (pid {pid} start {start})"),
        format!("claude agent agent-a1 (pid {pid} start )"),
        format!("claude agent agent-a1 (pid {pid} start {start}"),
        format!("claude agent agent-a1 (pid {pid} start {start}) by hand"),
        format!("claude agent agent-a1 (pid  {pid})"),
        format!("claude agent agent-a1 (pid {pid}0000000000)"),
        format!("claude agent agent-a1 (pid {pid}, start {start})"),
        "on a removable drive".to_owned(),
    ] {
        relock(&ws, &lib, &wt, &reason);
        assert_lib_feat(&ws, &live, &wt, None);
    }

    // the caller's own lock: its worktree is its own to act on
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {start})"),
    );
    let as_caller = read_as(&claude, &agent, true);
    assert_eq!(as_caller, LiveSessions::Known(vec![]));
    assert_lib_feat(&ws, &as_caller, &wt, None);
    // unless the process tree doesn't back the claim
    assert_lib_feat(&ws, &read_as(&claude, &agent, false), &wt, Some(&s));
}

#[test]
fn a_missing_worktree_claude_code_locked_is_busy_with_the_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    let (app, lib, wt) = lib_with_an_agent_worktree(&mut ws);
    let claude = claude_dir(&ws, "claude");
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    claude.session(pid, &start, &app);
    let s = child_session(&agent, &app, SessionSource::SessionFile);
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {start})"),
    );
    // its files gone, git keeps it for the lock
    std::fs::remove_dir_all(&wt).unwrap();
    let live = read(&claude);
    let run = ws.status_live(&live);
    let e = find_entry(&run.entries, "lib");
    assert_eq!(e.unprobed_worktrees.len(), 1, "{:?}", e.unprobed_worktrees);
    assert_eq!(e.unprobed_worktrees[0].worktree.why, UnprobedWhy::Missing);
    assert_lib_feat(&ws, &live, &wt, Some(&s));
    // a stale lock, its start another process's
    let reused = (start.parse::<u64>().unwrap() + 1).to_string();
    relock(
        &ws,
        &lib,
        &wt,
        &format!("claude agent agent-a1 (pid {pid} start {reused})"),
    );
    assert_lib_feat(&ws, &live, &wt, None);
}

#[test]
fn a_primary_claude_code_locked_is_busy_with_the_session_its_lock_names() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_repo("lib", "lib", "");
    let main_wt = ws.clone_owned("lib-main", "lib", &[]);
    ahead_branch(&ws, &main_wt, "feat");
    // the registry's dir is a linked worktree of the clone beside it
    let lib = ws.dir("lib");
    ws.add_worktree(&main_wt, &lib, &["feat"]);
    let claude = claude_dir(&ws, "claude");
    let agent = LiveChild::spawn();
    let (pid, start) = (agent.pid(), agent.proc_start());
    // at the workspace root: in no checkout
    claude.session(pid, &start, &ws.root());
    let s = child_session(&agent, &ws.root(), SessionSource::SessionFile);
    let live = read(&claude);
    assert_lib_feat(&ws, &live, &lib, None);
    relock(
        &ws,
        &main_wt,
        &lib,
        &format!("claude session feat (pid {pid} start {start})"),
    );
    let run = ws.status_live(&live);
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "lib");
    assert!(e.checkouts[0].primary && e.checkouts[0].locked);
    assert_lib_feat(&ws, &live, &lib, Some(&s));
}
