//! Exit codes and the error documents through the binary: caller errors,
//! runtime errors, and invalid registries. Run under the same hermetic
//! environment as the fixtures.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use fuz_repos::{PUSH_FORMAT_VERSION, STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use serde_json::Value;
use support::FixtureWorkspace;
use support::cli::{REPOS, error_doc, repos, stderr, stdout, workspace};

#[test]
fn a_workspace_root_that_cannot_be_listed_exits_one() {
    let ws = workspace();
    let root = ws.root();
    let Some(_unseal) = support::seal(&root, 0o311) else {
        return;
    };
    let out = repos(&ws, &root, &["status"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("failed to list the workspace root"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty());
    // with targets there's no scan, and no listing
    let out = repos(&ws, &root, &["status", "app"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

#[test]
fn caller_errors_exit_two() {
    let ws = workspace();
    let cases: [(&[&str], &str); 5] = [
        (
            &["status", "nope"],
            "error: unknown target `nope`\nhint: a target is",
        ),
        (
            &["status", "apq"],
            "error: unknown target `apq`\nhint: did you mean: app\n",
        ),
        (&["--root", "nowhere", "status"], "no workspace root"),
        (
            &["--registry", "missing.toml", "status"],
            "failed to read the registry",
        ),
        (&[], "a subcommand is required"),
    ];
    for (args, message) in cases {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stderr(&out).contains(message), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
    // an argument the parser rejects, `--json` or not: argh's text on
    // stderr, nothing on stdout
    for args in [&["status", "--bogus"][..], &["status", "--json", "--bogus"]] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&out).contains("--bogus"),
            "{args:?}: {}",
            stderr(&out)
        );
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
}

/// A `git` on `PATH` ahead of the real one that prints `version` for any
/// call; returns the `PATH` to run under.
fn fake_git(ws: &FixtureWorkspace, version: &str) -> std::ffi::OsString {
    let bin = ws.outside("fake-bin");
    support::write_executable(&bin, "git", &format!("#!/bin/sh\necho '{version}'\n"));
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn caller_errors_print_a_json_document() {
    let ws = workspace();
    let root = ws.root();
    let registry = root.join("repos.toml");

    // an unknown target, with its close keys
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "apq"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "unknown_entry",
            "name": "apq",
            "suggestions": ["app"],
            "message": "unknown target `apq`",
            "hint": "did you mean: app",
        })
    );
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "zzzzzz"]), 2);
    assert_eq!(error["suggestions"], serde_json::json!([]));
    assert_eq!(
        error["hint"],
        "a target is a registry key, an entry's dir name, or a path inside a checkout"
    );

    let error = error_doc(
        &repos(&ws, &root, &["--root", "nowhere", "status", "--json"]),
        2,
    );
    assert_eq!(error["kind"], "root_not_found");
    assert_eq!(
        error["message"],
        format!("no workspace root at {}", root.join("nowhere").display())
    );

    let error = error_doc(
        &repos(
            &ws,
            &root,
            &["--registry", "missing.toml", "status", "--json"],
        ),
        2,
    );
    assert_eq!(error["kind"], "registry_read");
    assert_eq!(error["hint"], Value::Null);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("failed to read the registry at "),
        "{error}"
    );

    // outside the workspace and outside any repo
    let plain = ws.outside("plain");
    std::fs::create_dir(&plain).unwrap();
    let error = error_doc(&repos(&ws, &plain, &["status", "--json"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "registry_not_found",
            "message": format!(
                "no repos.toml found in {} or any parent directory",
                plain.display()
            ),
            "hint": "run inside the workspace or a checkout of one of its repos, or pass \
                     `--registry <path>`",
        })
    );
    // in a repo outside the workspace, whose main checkout has no registry
    // above it either
    ws.remote("stray", &[]);
    let stray = ws.outside("stray");
    ws.git(
        ws.base(),
        &[
            "clone",
            "-q",
            &format!("file://{}", ws.bare("stray").display()),
            stray.to_str().unwrap(),
        ],
    );
    let error = error_doc(&repos(&ws, &stray, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");

    // git missing, or too old for `GIT_NO_LAZY_FETCH`
    let empty = ws.outside("empty-bin");
    std::fs::create_dir(&empty).unwrap();
    let out = ws
        .command(REPOS, &root)
        .env("PATH", &empty)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let error = error_doc(&out, 2);
    assert_eq!(error["kind"], "git_not_found");
    assert_eq!(error["message"], "git not found on PATH");
    // checked before discovery: outside the workspace too
    let out = ws
        .command(REPOS, &plain)
        .env("PATH", &empty)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(error_doc(&out, 2)["kind"], "git_not_found");
    for (version, found) in [
        ("git version 2.40.0", "2.40.0"),
        // a wrapper's banner before the version line
        ("wrapper banner\ngit version 2.40.1", "2.40.1"),
        ("git version 2.43.7 (Apple Git-150)", "2.43.7"),
        ("git version 2.43.0.windows.1", "2.43.0"),
        ("not a git at all", "not a git at all"),
    ] {
        let path = fake_git(&ws, version);
        let out = ws
            .command(REPOS, &root)
            .env("PATH", &path)
            .args(["status", "--json"])
            .output()
            .unwrap();
        let error = error_doc(&out, 2);
        assert_eq!(
            error,
            serde_json::json!({
                "kind": "git_too_old",
                "found": found,
                "required": "2.44.0",
                "message": format!("git reports version `{found}`; repos needs 2.44.0 or newer"),
                "hint": "repos sets `GIT_NO_LAZY_FETCH` (git 2.44+) so a local call on a \
                         partial clone never touches the network — upgrade git",
            }),
            "{version}"
        );
        // text mode: the same lines, nothing on stdout
        let out = ws
            .command(REPOS, &root)
            .env("PATH", &path)
            .args(["status"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(stdout(&out).is_empty());
        assert!(
            stderr(&out).contains("GIT_NO_LAZY_FETCH"),
            "{}",
            stderr(&out)
        );
    }
    // git before 2.15 rejects the runner's `--no-optional-locks` before it
    // can print a version
    let bin = ws.outside("ancient-bin");
    support::write_executable(
        &bin,
        "git",
        "#!/bin/sh\necho \"Unknown option: $1\" >&2\necho 'usage: git [--version] <command>' >&2\n\
         exit 129\n",
    );
    let out = ws
        .command(REPOS, &root)
        .env("PATH", &bin)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let error = error_doc(&out, 2);
    assert_eq!(error["kind"], "git_too_old");
    assert_eq!(error["found"], "unknown (older than 2.15)");
    assert!(!stderr(&out).contains("usage:"), "{}", stderr(&out));
    // a new enough fake passes the check: whatever fails next isn't it
    let out = ws
        .command(REPOS, &root)
        .env("PATH", fake_git(&ws, "git version 2.44.0.windows.1"))
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        !stderr(&out).contains("git reports version"),
        "{}",
        stderr(&out)
    );

    // last: it rewrites the registry
    std::fs::write(
        &registry,
        "owners = [\"me\"]\n[repos.app]\nurl = \"https://github.com/me/app\"\nbogus = 1\n",
    )
    .unwrap();
    let error = error_doc(&repos(&ws, &root, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_parse");
    assert!(
        error["message"].as_str().unwrap().contains("bogus"),
        "{error}"
    );
}

#[test]
fn a_non_utf8_argument_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let ws = workspace();
    let arg = std::ffi::OsStr::from_bytes(b"a\xffb");
    let s = std::ffi::OsStr::new;
    // a target, and a flag's value
    for args in [
        [s("status"), s("--json"), arg],
        [s("--registry"), arg, s("status")],
    ] {
        let out = ws.command(REPOS, &ws.root()).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
        assert!(stdout(&out).is_empty());
        assert_eq!(
            stderr(&out),
            "error: argument `a\u{fffd}b` is not valid UTF-8; repos takes UTF-8 arguments \
             only (paths included)\n"
        );
    }
}

#[test]
fn a_runtime_error_prints_a_json_document() {
    let ws = workspace();
    let root = ws.root();
    let Some(_unseal) = support::seal(&root, 0o311) else {
        return;
    };
    let error = error_doc(&repos(&ws, &root, &["status", "--json"]), 1);
    assert_eq!(error["kind"], "io");
    assert_eq!(error["hint"], Value::Null);
    assert!(
        error["message"].as_str().unwrap().starts_with(&format!(
            "failed to list the workspace root {}: ",
            root.display()
        )),
        "{error}"
    );
}

#[test]
fn an_invalid_registry_exits_two() {
    let ws = workspace();
    std::fs::write(
        ws.root().join("repos.toml"),
        "owners = [\"me\"]\n[repos.app]\nurl = \"https://github.com/me/app\"\nbogus = 1\n",
    )
    .unwrap();
    let out = repos(&ws, &ws.root(), &["status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("bogus"), "{}", stderr(&out));
}

#[test]
fn an_integrity_issue_is_a_registry_invalid_document() {
    let ws = workspace();
    let root = ws.root();
    let registry = root.join("repos.toml");
    std::fs::write(
        &registry,
        r#"owners = ["me"]
[repos.app]
url = "https://github.com/me/app"
visibility = "public"
purpose = "x"
requires = ["spec"]
[repos.theirs]
url = "https://github.com/them/theirs"
dir = "app"
visibility = "public"
purpose = "x"
"#,
    )
    .unwrap();
    let message = format!(
        "invalid registry at {}:\n  \
         repo `theirs` sits under `them`, not an owner — a third-party clone belongs in \
         [references]\n  \
         repo `theirs` claims dir `app`, already claimed by repo `app`\n  \
         repo `app` requires `spec`, which is neither a repo nor a reference",
        registry.display()
    );
    let hint = "fix each issue in the registry; nothing is probed until it validates";
    // validated before targets resolve: the unknown one is never looked at
    let error = error_doc(&repos(&ws, &root, &["status", "--json", "nope"]), 2);
    assert_eq!(
        error,
        serde_json::json!({
            "kind": "registry_invalid",
            "issues": [
                {"kind": "repo_not_owned", "key": "theirs", "account": "them"},
                {
                    "kind": "dir_claimed_twice",
                    "dir": "app",
                    "first": {"kind": "repo", "key": "app"},
                    "second": {"kind": "repo", "key": "theirs"},
                },
                {
                    "kind": "unknown_checkout_ref",
                    "key": "app",
                    "field": "requires",
                    "target": "spec",
                },
            ],
            "message": message,
            "hint": hint,
        })
    );
    // text: every issue on stderr, nothing on stdout
    let out = repos(&ws, &root, &["status"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert_eq!(stderr(&out), format!("error: {message}\nhint: {hint}\n"));
}

#[test]
fn version_and_help_exit_zero() {
    let ws = workspace();
    let out = repos(&ws, &ws.root(), &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.starts_with(&format!("repos {} (", env!("CARGO_PKG_VERSION"))),
        "{text}"
    );
    // the build's identity, then each `--json` document's version
    let formats = format!(
        ") · formats: status {STATUS_FORMAT_VERSION}, sync {SYNC_FORMAT_VERSION}, \
         push {PUSH_FORMAT_VERSION}\n"
    );
    assert!(
        text.ends_with(&formats) && text.lines().count() == 1,
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("status"));
}
