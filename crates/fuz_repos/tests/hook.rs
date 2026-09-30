//! `repos hook pre-tool-use` as Claude Code runs it: the hook's JSON on
//! stdin, the verdict in the exit code and output. Each run has an empty
//! environment (no `PATH`, so no git) and a cwd with no registry above it:
//! the hook reads stdin alone.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use fuz_repos::hook::Denial;

const REPOS: &str = env!("CARGO_BIN_EXE_repos");

/// Runs the hook with `input` on stdin.
fn hook(input: &[u8]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(REPOS)
        .args(["hook", "pre-tool-use"])
        .current_dir(dir.path())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// A Bash call's `PreToolUse` input, as Claude Code writes it.
fn bash(command: &str) -> Vec<u8> {
    serde_json::json!({
        "session_id": "0b5c5e3a-1f4e-4c43-9d7e-2a8f1c7b9e10",
        "transcript_path": "/home/me/.claude/projects/x/0b5c5e3a.jsonl",
        "cwd": "/home/me/dev/app",
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command, "description": "Push the branch"},
        "tool_use_id": "toolu_01",
    })
    .to_string()
    .into_bytes()
}

#[track_caller]
fn assert_denied(out: &Output, denial: Denial) {
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": denial.reason(),
            }
        })
    );
    assert!(out.stdout.ends_with(b"}\n"));
    // the settings' guard blocks only on this exact text
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("{}\n", denial.reason())
    );
}

#[track_caller]
fn assert_silent(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
}

#[test]
fn a_raw_push_is_denied() {
    for command in [
        "git push",
        "git -C ../app push origin main",
        "cd ../app && bash -c 'git push'",
    ] {
        assert_denied(&hook(&bash(command)), Denial::GitPush);
    }
    assert_denied(&hook(&bash("git push 'origin")), Denial::Unreadable);
}

#[test]
fn what_is_the_users_is_denied() {
    assert_denied(
        &hook(&bash("repos push gro --new-branch")),
        Denial::NewBranch,
    );
    assert_denied(
        &hook(&bash("env -u CLAUDECODE repos push")),
        Denial::Claudecode,
    );
}

#[test]
fn everything_else_passes_in_silence() {
    for command in [
        "git status",
        "git stash push",
        "repos push",
        "echo \"git push\"",
    ] {
        assert_silent(&hook(&bash(command)));
    }
}

#[test]
fn input_it_cannot_read_passes_in_silence() {
    for input in [
        &b""[..],
        b"not json",
        b"{\"tool_name\": \"Bash\", \"tool_input\": ",
        b"{\"tool_name\": \"Bash\", \"tool_input\": {}}",
        b"{\"tool_name\": \"Write\", \"tool_input\": {\"command\": \"git push\"}}",
        b"\xff\xfe",
    ] {
        assert_silent(&hook(input));
    }
}
