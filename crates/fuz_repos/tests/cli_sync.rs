//! `repos sync` through the binary: the outcome report, the summary text, and
//! exit codes. Run under the same hermetic environment as the fixtures.

mod support;

use fuz_repos::{STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};
use serde_json::Value;
use support::FixtureWorkspace;
use support::cli::{REPOS, parse, repos, stderr, stdout, workspace};

/// `app` behind by one, `blog` ahead by one, `gone` missing, its remote
/// there to clone.
fn sync_workspace() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    let blog = ws.owned_repo("blog", &[]);
    ws.commit(&blog, "local");
    ws.assert_track(&blog, "main", "[ahead 1]");
    ws.remote("gone", &[]);
    ws.declare_repo("gone", "gone", "");
    ws.write_registry();
    ws.assert_track(&app, "main", "");
    assert!(!ws.dir("gone").exists());
    ws
}

#[test]
fn sync_json_is_the_versioned_outcome_report() {
    let ws = sync_workspace();
    let tip = ws.git(&ws.bare("app"), &["rev-parse", "main"]);
    let blog_was = ws.git(&ws.bare("blog"), &["rev-parse", "main"]);
    let blog_tip = ws.git(&ws.dir("blog"), &["rev-parse", "main"]);
    let report = parse(&repos(&ws, &ws.root(), &["sync", "--json"]));
    assert_eq!(report["version"], SYNC_FORMAT_VERSION);
    assert_eq!(report["status"]["version"], STATUS_FORMAT_VERSION);
    assert_eq!(report["status"]["fetched"], true);
    // no targets: the scan ran, before anything was cloned, and found no
    // stray (the clone it made is registered)
    assert_eq!(report["status"]["unregistered"], serde_json::json!([]));
    let entries = report["entries"].as_array().unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["app", "blog", "gone"]);
    assert_eq!(entries[0]["fetch"], serde_json::json!({"kind": "fetched"}));
    assert_eq!(entries[0]["branches"][0]["name"], "main");
    assert_eq!(entries[0]["branches"][0]["kind"], "fast_forwarded");
    assert_eq!(entries[0]["branches"][0]["to"], tip.as_str());
    assert_eq!(
        entries[1]["branches"][0],
        serde_json::json!({
            "name": "main",
            "kind": "pushed",
            "from": blog_was,
            "to": blog_tip,
            "repeats": null,
        })
    );
    let gone_tip = ws.git(&ws.bare("gone"), &["rev-parse", "main"]);
    assert_eq!(
        entries[2],
        serde_json::json!({
            "key": "gone",
            "fetch": {"kind": "not_fetched"},
            "clone": {"kind": "cloned", "branch": "main", "head": gone_tip},
            "branches": [],
        })
    );
    assert_eq!(ws.git(&ws.dir("gone"), &["rev-parse", "HEAD"]), gone_tip);
    assert_eq!(ws.git(&ws.dir("app"), &["rev-parse", "main"]), tip);
    assert_eq!(ws.git(&ws.bare("blog"), &["rev-parse", "main"]), blog_tip);
}

#[test]
fn an_agents_sync_pushes_as_a_persons_does() {
    let ws = sync_workspace();
    let blog_was = ws.git(&ws.bare("blog"), &["rev-parse", "main"]);
    let agent = |args: &[&str]| {
        ws.command(REPOS, &ws.root())
            .env("CLAUDECODE", "1")
            .args(args)
            .output()
            .unwrap()
    };
    // the preview holds nothing for it
    let text = stdout(&agent(&["status"]));
    assert!(
        text.starts_with("sync would    push blog +1 · clone gone\n"),
        "{text}"
    );
    let out = agent(&["sync"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        ["synced        push blog +1 · ff app −1 · clone gone"],
        "{text}"
    );
    ws.assert_head(&ws.dir("gone"), Some("main"));
    let blog_tip = ws.git(&ws.dir("blog"), &["rev-parse", "main"]);
    assert_ne!(blog_tip, blog_was);
    assert_eq!(ws.git(&ws.bare("blog"), &["rev-parse", "main"]), blog_tip);
}

#[test]
fn sync_text_is_the_summary_with_what_it_did() {
    let ws = sync_workspace();
    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        ["synced        push blog +1 · ff app −1 · clone gone"],
        "{text}"
    );
    assert!(
        lines[lines.len() - 1].starts_with("clean 0 · on branches 0 · pinned 0"),
        "{text}"
    );

    // again: nothing left to push, fast-forward, or clone
    let text = stdout(&repos(&ws, &ws.root(), &["sync"]));
    assert!(
        text.starts_with("clean 3 · on branches 0 · pinned 0"),
        "{text}"
    );
    // status agrees
    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with("clean 3 · on branches 0 · pinned 0"),
        "{text}"
    );
}

#[test]
fn a_failed_clone_fails_the_run() {
    // `gone` has no remote to clone
    let ws = workspace();
    let text = stdout(&repos(&ws, &ws.root(), &["status", "--verbose"]));
    assert!(
        text.contains(
            "gone  repo · owned · public · ci · follow main\n  \
             url       https://github.com/me/gone\n  \
             dir       missing: gone\n  \
             clone     git@github.com:me/gone · branch main\n"
        ),
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("failed        gone (clone: repo not found)\nsynced        push app +1\n"),
        "{text}"
    );
    assert!(!ws.dir("gone").exists());
}

#[test]
fn sync_text_says_an_action_once_for_entries_sharing_a_repo() {
    // `app_wt` is a linked worktree of app's, on `wt`: both entries list
    // both branches, each acted on once, for the repo
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["branch", "-q", "--track", "wt", "origin/main"]);
    ws.add_worktree(&app, &ws.dir("app-wt"), &["wt"]);
    ws.declare_repo("app_wt", "app", "dir = \"app-wt\"");
    ws.upstream_commit("app", "main");
    ws.write_registry();

    let report = parse(&repos(&ws, &ws.root(), &["sync", "--json"]));
    let repeats = |e: usize| -> Vec<Value> {
        report["entries"][e]["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["repeats"].clone())
            .collect()
    };
    // per entry in the report: app's own, app_wt's app's
    assert_eq!(repeats(0), [Value::Null, Value::Null]);
    assert_eq!(repeats(1), ["app", "app"]);
    for e in 0..2 {
        for b in 0..2 {
            assert_eq!(
                report["entries"][e]["branches"][b]["kind"],
                "fast_forwarded"
            );
        }
    }

    // the summary says each once
    ws.upstream_commit("app", "main");
    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("synced        ff app −1, app:wt −1\n"),
        "{text}"
    );
}

#[test]
fn sync_exits_one_when_git_refuses_an_action() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[(".gitignore", "secret.env\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    let up = ws.upstream("app");
    support::write(&up, "secret.env", "tracked\n");
    ws.git(&up, &["add", "-f", "secret.env"]);
    ws.git(&up, &["commit", "-q", "-m", "track it"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    support::write(&app, "secret.env", "mine\n");
    ws.assert_clean(&app);
    ws.write_registry();
    let head = ws.git(&app, &["rev-parse", "HEAD"]);

    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "failed        app (ff: error: The following untracked working tree files would \
             be overwritten by merge: secret.env)\n"
        ),
        "{}",
        stdout(&out)
    );
    // `--json` prints the report, failure and all
    let out = repos(&ws, &ws.root(), &["sync", "--json"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let report: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(report["entries"][0]["branches"][0]["kind"], "failed");
    assert_eq!(ws.git(&app, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read_to_string(app.join("secret.env")).unwrap(),
        "mine\n"
    );
}

#[test]
fn sync_caller_errors_exit_two_with_a_sync_document() {
    let ws = sync_workspace();
    let out = repos(&ws, &ws.root(), &["sync", "--json", "nope"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(doc["version"], SYNC_FORMAT_VERSION);
    assert_eq!(doc["error"]["kind"], "unknown_entry");
    // nothing was fetched: the target failed before anything ran
    assert_eq!(
        ws.git(&ws.dir("app"), &["rev-parse", "origin/main"]),
        ws.git(&ws.dir("app"), &["rev-parse", "main"])
    );
}

#[test]
fn push_refusals_read_in_the_summary() {
    // `app` pushes to another repo; `blog`'s remote refuses the push
    let mut ws = FixtureWorkspace::new();
    ws.remote("other", &[]);
    let app = ws.owned_repo("app", &[]);
    ws.commit(&app, "local");
    ws.git(
        &app,
        &["config", "remote.origin.pushurl", "git@github.com:me/other"],
    );
    let blog = ws.owned_repo("blog", &[]);
    ws.commit(&blog, "local");
    support::write_executable(
        &ws.bare("blog"),
        "hooks/pre-receive",
        "#!/bin/sh\necho 'error: GH006: Protected branch update failed.' >&2\nexit 1\n",
    );
    ws.write_registry();

    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with(
            "needs human   app (push goes to git@github.com:me/other)\n\
             sync would    push blog +1\n\
             held          push app +1 (push URL)\n"
        ),
        "{text}"
    );
    let text = stdout(&repos(&ws, &ws.root(), &["status", "--verbose", "app"]));
    assert!(
        text.contains(
            "  needs     push goes to git@github.com:me/other — sync pushes only to \
             git@github.com:me/app: see git -C "
        ),
        "{text}"
    );

    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(
            "failed        blog (push: rejected: GH006: Protected branch update failed.)\n\
             needs human   app (push goes to git@github.com:me/other)\n\
             held          push app +1 (push URL)\n"
        ),
        "{text}"
    );
}

/// A fetch a rewrite sends to another repo: said, and its fix, the
/// rewrite to look at — the command as printed lists it.
#[test]
fn a_fetch_url_elsewhere_reads_with_its_rewrite() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(
        &app,
        &[
            "config",
            "url.git@github.com:me/other.insteadOf",
            "git@github.com:me/app",
        ],
    );
    ws.write_registry();

    let before = ws.refs(&app);

    let out = repos(&ws, &ws.root(), &["status", "--fetch"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("needs human   app (fetch goes to git@github.com:me/other)\n"),
        "{}",
        stdout(&out)
    );
    // never fetched
    assert_eq!(ws.refs(&app), before);
    assert_eq!(ws.ssh_log(), Vec::<String>::new());
    let text = stdout(&repos(&ws, &ws.root(), &["status", "--verbose", "app"]));
    let advice = format!(
        "  needs     fetch goes to git@github.com:me/other — sync fetches only the registry's \
         repo, git@github.com:me/app: a url.*.insteadOf rewrite makes it: see git -C {} config \
         --get-regexp '^url\\..*\\.insteadof$'\n",
        app.display()
    );
    assert!(text.contains(&advice), "{text}");
    // run as printed, it lists the rewrite
    let listed = ws.git(&app, &["config", "--get-regexp", "^url\\..*\\.insteadof$"]);
    assert_eq!(
        listed,
        "url.git@github.com:me/other.insteadof git@github.com:me/app"
    );
}
