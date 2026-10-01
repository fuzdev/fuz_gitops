//! `repos push` through the binary: exit codes, the outcome document, and usage
//! errors. Run under the same hermetic environment as the fixtures.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::process::Output;

use fuz_repos::PUSH_FORMAT_VERSION;
use fuz_repos::report::{BranchSyncHold, FetchOutcome, PushOutcome};
use fuz_repos::state::{BranchHold, Relation, SyncAction, Verdict};
use serde_json::Value;
use support::cli::{REPOS, repos, stderr, stdout};
use support::push::{ahead, assert_tracks_origin, only, pushes_served, remote_refs, topic};
use support::{FixtureWorkspace, THIRD_PARTY, branch, find_entry};

/// `repos push --json`'s fatal-error document, after checking the exit.
fn error_doc(out: &Output, code: i32) -> Value {
    assert_eq!(out.status.code(), Some(code), "stderr: {}", stderr(out));
    let doc: Value = serde_json::from_str(&stdout(out)).unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    doc
}

#[test]
fn push_from_the_cwd_exits_zero_once_in_sync() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    std::fs::create_dir(app.join("sub")).unwrap();

    let out = repos(&ws, &app.join("sub"), &["push"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "pushed        app +1", "{text}");
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), tip);

    let out = repos(&ws, &app, &["push"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("in sync       app\n"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn push_exits_one_when_a_branch_isnt_pushed() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    let blog = ws.owned_repo("blog", &[]);
    ws.git(&blog, &["switch", "-q", "-c", "topic"]);
    ws.commit(&blog, "topic");
    ws.assert_upstream(&blog, "topic", "");
    ws.write_registry();
    let app_was = ws.git(&app, &["rev-parse", "main"]);

    let out = repos(&ws, &ws.root(), &["push", "app", "blog"]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(
            "not pushed    app (behind 1)  blog:topic (no upstream on origin)\n              \
             hint: repos sync fast-forwards a branch behind its upstream (and moves a stale \
             shallow one)\n              \
             hint: the user creates it on origin with repos push --new-branch (an agent \
             can't)\n"
        ),
        "{text}"
    );
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), app_was);
    ws.assert_head(&blog, Some("topic"));
    assert!(!ws.has_ref(&ws.bare("blog"), "refs/heads/topic"));

    let doc: Value = serde_json::from_str(&stdout(&repos(
        &ws,
        &ws.root(),
        &["push", "--json", "app", "blog"],
    )))
    .unwrap();
    let kinds: Vec<&str> = doc["pushes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["not_ahead", "no_upstream"]);
}

#[test]
fn a_branch_in_sync_on_refs_that_may_not_be_origins_is_held() {
    // (setup on a workspace with `app` in sync, the hold it reads, its text)
    type Unverified = fn(&FixtureWorkspace, &Path);
    let cases: [(&str, Unverified, &str, &str); 2] = [
        (
            "fetch failed",
            |ws, _| std::fs::remove_dir_all(ws.bare("app")).unwrap(),
            "fetch_failed",
            "held          app (fetch failed)",
        ),
        (
            "origin drift",
            |ws, app| {
                // another repo with the same history, the branch in sync
                // with it
                let from = format!("file://{}", ws.bare("app").display());
                let other = ws.bare("other");
                ws.git(
                    ws.base(),
                    &["clone", "-q", "--bare", &from, other.to_str().unwrap()],
                );
                ws.set_origin(app, "other", "git@github.com:me/other");
                ws.git(app, &["fetch", "-q", "origin"]);
                ws.assert_track(app, "main", "");
            },
            "entry",
            "held          app",
        ),
    ];
    for (case, unverify, by, held) in cases {
        let mut ws = FixtureWorkspace::new();
        let app = ws.owned_repo("app", &[]);
        ws.write_registry();
        ws.assert_track(&app, "main", "");
        unverify(&ws, &app);

        let out = repos(&ws, &ws.root(), &["push", "--json", "app"]);

        assert_eq!(out.status.code(), Some(1), "{case}: {}", stderr(&out));
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        let p = &doc["pushes"][0];
        assert_eq!(
            (&p["kind"], &p["by"]),
            (&"held".into(), &by.into()),
            "{case}"
        );
        let out = repos(&ws, &ws.root(), &["push", "app"]);
        assert_eq!(out.status.code(), Some(1), "{case}");
        let text = stdout(&out);
        assert!(text.lines().any(|l| l == held), "{case}: {text}");
        assert!(!text.contains("in sync"), "{case}: {text}");
        assert_eq!(pushes_served(&ws), Vec::<String>::new(), "{case}");
    }
}

#[test]
fn a_held_push_keeps_syncs_label_whatever_else_holds_it() {
    // ahead, origin pushing elsewhere, and the fetch failing: sync names
    // the push URL first, and so does the push
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    ws.git(
        &app,
        &["config", "remote.origin.pushurl", "git@github.com:me/other"],
    );
    std::fs::remove_dir_all(ws.bare("app")).unwrap();

    let run = ws.push(&["app"]);

    let e = find_entry(&run.entries, "app");
    assert!(e.fetch_error.is_some());
    assert_eq!(
        branch(e, "main").verdict,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: BranchHold::PushUrl,
        }
    );
    assert_eq!(
        only(&run),
        (
            Some("main"),
            &PushOutcome::Held {
                by: BranchSyncHold::PushUrl
            }
        )
    );
    assert!(matches!(run.pushes[0].fetch, FetchOutcome::Failed { .. }));
    assert_eq!(pushes_served(&ws), Vec::<String>::new());
}

#[test]
fn push_json_is_the_versioned_outcome_report() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let from = remote_refs(&ws, "app")["refs/heads/main"].clone();

    let out = repos(&ws, &ws.root(), &["push", "--json", "app"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    assert_eq!(doc["status"]["version"], fuz_repos::STATUS_FORMAT_VERSION);
    assert_eq!(doc["status"]["fetched"], true);
    assert_eq!(doc["status"]["unregistered"], Value::Null);
    assert_eq!(
        doc["pushes"],
        serde_json::json!([{
            "key": "app",
            "checkout": app.to_str().unwrap(),
            "branch": "main",
            "fetch": {"kind": "fetched"},
            "kind": "pushed",
            "from": from,
            "to": tip,
        }])
    );
}

#[test]
fn push_usage_errors_exit_two() {
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    // an owned reference, pinned
    ws.remote("wpt", &[]);
    ws.declare_reference("wpt", support::OWNER, "wpt", "pinned = true");
    ws.clone_owned("wpt", "wpt", &[]);
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party("lib", "lib", &[]);
    ws.write_registry();
    let ssh_before = ws.ssh_log();

    // the cwd in no entry's checkout
    let out = repos(&ws, &ws.root(), &["push"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out).lines().next().unwrap(),
        format!(
            "error: {} is in no registry entry's checkout",
            ws.root().display()
        )
    );
    let doc = error_doc(&repos(&ws, &ws.root(), &["push", "--json"]), 2);
    assert_eq!(doc["error"]["kind"], "no_checkout");
    // an unknown target
    let doc = error_doc(&repos(&ws, &ws.root(), &["push", "--json", "ap"]), 2);
    assert_eq!(doc["error"]["kind"], "unknown_entry");
    // a third-party reference, by key or from inside it, and a pin
    for (cwd, target) in [(ws.root(), "lib"), (lib, ".")] {
        let doc = error_doc(&repos(&ws, &cwd, &["push", "--json", target]), 2);
        assert_eq!(
            doc["error"],
            serde_json::json!({
                "kind": "push_third_party",
                "key": "lib",
                "message": "`lib` is a third-party reference, which repos never pushes",
                "hint": "repos push takes the registry's owned repos; a reference's commits \
                         stay local",
            })
        );
    }
    let doc = error_doc(
        &repos(&ws, &ws.root(), &["push", "--json", "app", "wpt"]),
        2,
    );
    assert_eq!(doc["error"]["kind"], "push_pinned");
    assert_eq!(doc["error"]["key"], "wpt");
    // refused before anything reached a remote
    assert_eq!(ws.ssh_log(), ssh_before);
}

#[test]
fn an_agent_pushes_through_push() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = ahead(&mut ws);
    let blog = ws.owned_repo("blog", &[]);
    ws.commit(&blog, "local");
    ws.write_registry();
    let blog_was = ws.git(&ws.bare("blog"), &["rev-parse", "main"]);

    let out = ws
        .command(REPOS, &app)
        .env("CLAUDECODE", "1")
        .args(["push"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("pushed        app +1\n"),
        "{}",
        stdout(&out)
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "main"]), tip);
    // the other repo, only sync's to push
    assert_eq!(ws.git(&ws.bare("blog"), &["rev-parse", "main"]), blog_was);
}

#[test]
fn a_pushed_status_reads_in_sync_without_a_fetch() {
    let mut ws = FixtureWorkspace::new();
    let (app, _) = ahead(&mut ws);
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "main").relation, Relation::Ahead { commits: 1 });

    let run = ws.push(&["app"]);
    assert!(matches!(only(&run).1, PushOutcome::Pushed { .. }));

    // local refs only: the remote-tracking ref the push moved
    let e = find_entry(&ws.status(), "app").clone();
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
    assert_eq!(branch(&e, "main").verdict, Verdict::Quiet);
    ws.assert_track(&app, "main", "");
}

#[test]
fn new_branch_is_the_users_and_refused_to_an_agent() {
    let mut ws = FixtureWorkspace::new();
    let (app, tip) = topic(&mut ws);
    let ssh_before = ws.ssh_log();
    let agent = |args: &[&str]| {
        ws.command(REPOS, &app)
            .env("CLAUDECODE", "1")
            .args(args)
            .output()
            .unwrap()
    };

    let out = agent(&["push", "--new-branch"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "error: creating a remote branch is the user's: repos push --new-branch doesn't run \
         in an agent's shell (CLAUDECODE is set)\n\
         hint: the user runs repos push --new-branch themselves; an agent pushes a branch \
         origin already has with repos push\n"
    );
    let doc = error_doc(&agent(&["push", "--new-branch", "--json"]), 2);
    assert_eq!(doc["error"]["kind"], "new_branch_by_agent");
    // refused before anything ran: not even the fetch
    assert_eq!(ws.ssh_log(), ssh_before);
    // the agent's own push says whose it is
    let out = agent(&["push"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "not pushed    app:topic (no upstream on origin)\n              hint: the user \
             creates it on origin with repos push --new-branch (an agent can't)\n"
        ),
        "{}",
        stdout(&out)
    );
    assert!(!ws.has_ref(&ws.bare("app"), "refs/heads/topic"));

    // the user's
    let out = repos(&ws, &app, &["push", "--new-branch"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(
        text.lines().next(),
        Some("pushed        app:topic (new branch)"),
        "{text}"
    );
    assert_eq!(ws.git(&ws.bare("app"), &["rev-parse", "topic"]), tip);
    assert_tracks_origin(&ws, &app, "topic", &tip);
    let doc: Value = serde_json::from_str(&stdout(&repos(
        &ws,
        &app,
        &["push", "--json", "--new-branch"],
    )))
    .unwrap();
    assert_eq!(doc["version"], PUSH_FORMAT_VERSION);
    assert_eq!(doc["pushes"][0]["kind"], "in_sync");
}
