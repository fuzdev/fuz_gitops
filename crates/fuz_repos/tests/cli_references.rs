//! Third-party references through the binary: refreshed only when named or
//! under `--references`. Run under the same hermetic environment as the
//! fixtures.

mod support;

use serde_json::Value;
use support::cli::{parse, repos, stderr, stdout};
use support::{FixtureWorkspace, THIRD_PARTY};

/// `app` in sync; `lib`, a third-party reference behind upstream, `origin`
/// its HTTPS URL, served by the fixture's `https`; `oracle`, a third-party
/// pin.
fn references_workspace() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party_over_https("lib", "lib", &[]);
    let lib_tip = ws.upstream_commit("lib", "main");
    ws.remote("oracle", &[]);
    ws.declare_reference("oracle", THIRD_PARTY, "oracle", "pinned = true");
    let oracle = ws.clone_third_party_over_https("oracle", "oracle", &[]);
    let oracle_tip = ws.upstream_commit("oracle", "main");
    // each behind once a fetch reaches it
    ws.assert_behind_at_remote(&lib, "lib", "main", &lib_tip);
    ws.assert_behind_at_remote(&oracle, "oracle", "main", &oracle_tip);
    ws.serve_https();
    ws.write_registry();
    ws
}

#[test]
fn a_reference_is_refreshed_only_when_named_or_under_references() {
    let ws = references_workspace();
    let lib_url = format!("https://github.com/{THIRD_PARTY}/lib");

    // by default: never fetched, quiet
    let text = stdout(&repos(&ws, &ws.root(), &["sync"]));
    assert!(
        text.starts_with("clean 2 · on branches 0 · pinned 1"),
        "{text}"
    );
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());

    // the preview, from local refs: nothing fetched
    let text = stdout(&repos(&ws, &ws.root(), &["status", "--references"]));
    assert!(
        text.starts_with("sync would    refresh lib\nclean 1 · on branches 0 · pinned 1"),
        "{text}"
    );
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    // with `--fetch`: fetched, the pin not
    let text = stdout(&repos(
        &ws,
        &ws.root(),
        &["status", "--fetch", "--references"],
    ));
    assert!(
        text.starts_with("sync would    refresh lib · ff lib:main −1\n"),
        "{text}"
    );
    assert_eq!(ws.https_log(), [lib_url.as_str()]);

    // named: refreshed; the pin named, refused
    let out = repos(&ws, &ws.root(), &["sync", "lib", "oracle"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(
            "synced        refresh lib · ff lib:main −1\nheld          refresh oracle (pinned)\n"
        ),
        "{text}"
    );
    assert_eq!(ws.https_log(), [lib_url.as_str(), lib_url.as_str()]);
    assert_eq!(
        ws.git(&ws.dir("lib"), &["rev-parse", "main"]),
        ws.git(&ws.bare("lib"), &["rev-parse", "main"])
    );

    // `--references`, as JSON: each entry's refresh, carried out as its fetch
    ws.upstream_commit("lib", "main");
    let report = parse(&repos(&ws, &ws.root(), &["sync", "--references", "--json"]));
    let status = report["status"]["entries"].as_array().unwrap();
    let keys: Vec<&str> = status.iter().map(|e| e["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["app", "lib", "oracle"]);
    assert_eq!(status[0]["refresh"], Value::Null);
    assert_eq!(status[1]["refresh"], serde_json::json!({"kind": "act"}));
    // a pin no one named: quiet, never fetched
    assert_eq!(status[2]["refresh"], Value::Null);
    let fetch = |i: usize| report["entries"][i]["fetch"]["kind"].clone();
    assert_eq!(
        [fetch(0), fetch(1), fetch(2)],
        ["fetched", "fetched", "not_fetched"]
    );
    assert_eq!(
        report["entries"][1]["branches"][0]["kind"],
        "fast_forwarded"
    );
    // named in `status`: the pin's refusal previewed
    let report = parse(&repos(&ws, &ws.root(), &["status", "oracle", "--json"]));
    assert_eq!(
        report["entries"][0]["refresh"],
        serde_json::json!({"kind": "held", "by": "pinned"})
    );
}

#[test]
fn a_missing_entry_cloned_under_another_name_is_held_when_the_scan_runs() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.clone_owned("app-old", "app", &[]);
    ws.write_registry();

    let text = stdout(&repos(&ws, &ws.root(), &["status"]));
    assert!(
        text.starts_with(
            "needs human   app (already cloned as app-old, not cloned)\nheld          clone app\n"
        ),
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["sync"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(
            "needs human   app (already cloned as app-old, not cloned)\nheld          clone app\n"
        ),
        "{}",
        stdout(&out)
    );
    assert!(!ws.dir("app").exists());
    // named, missing: the scan runs all the same, and holds it
    let text = stdout(&repos(&ws, &ws.root(), &["status", "app"]));
    assert!(
        text.starts_with(
            "needs human   app (already cloned as app-old, not cloned)\nheld          clone app\n"
        ),
        "{text}"
    );
    let out = repos(&ws, &ws.root(), &["sync", "app"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with("needs human   app (already cloned as app-old, not cloned)\n"),
        "{}",
        stdout(&out)
    );
    assert!(!ws.dir("app").exists());
    // a targeted run reports no unregistered dirs: it's about the named
    // entries
    let report = parse(&repos(&ws, &ws.root(), &["status", "app", "--json"]));
    assert_eq!(report["unregistered"], Value::Null);
}

/// A repo renamed from `_` to `-`, its checkout still under the old name:
/// held in a run naming it as in one naming none.
#[test]
fn a_missing_entry_cloned_under_its_old_name_is_held_named_or_not() {
    let mut ws = FixtureWorkspace::new();
    let name = "vscode-extension-tsv-format";
    ws.remote(name, &[]);
    ws.declare_repo(name, name, "");
    ws.clone_as(
        "vscode_extension_tsv_format",
        name,
        "git@github.com:me/vscode_extension_tsv_format",
        &[],
    );
    ws.write_registry();
    let held = format!(
        "needs human   {name} (already cloned as vscode_extension_tsv_format, not cloned)\n\
         held          clone {name}\n"
    );

    for args in [
        &["status"][..],
        &["status", name],
        &["sync"],
        &["sync", name],
    ] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        assert!(
            stdout(&out).starts_with(&held),
            "{args:?}: {}",
            stdout(&out)
        );
        assert!(!ws.dir(name).exists(), "{args:?}");
    }
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

/// `kit` as a person's checkout has it: a third-party reference whose
/// `origin` is the owner's SSH fork, no rewrite.
#[test]
fn a_reference_with_origin_drift_is_never_refreshed() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("kit", &[]);
    ws.declare_reference("kit", THIRD_PARTY, "kit", "");
    let kit = ws.clone_third_party_over_https("kit", "kit", &[]);
    ws.git(
        &kit,
        &["remote", "set-url", "origin", "git@github.com:me/kit"],
    );
    ws.upstream_commit("kit", "main");
    ws.serve_https();
    ws.write_registry();
    let before = ws.refs(&kit);

    for args in [
        &["sync", "kit"][..],
        &["sync", "--references"],
        &["status", "kit"],
        &["status", "--references", "--fetch"],
    ] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        let text = stdout(&out);
        // the drift and its fix, then the refresh it holds
        assert!(
            text.starts_with(
                "origin drift  kit (me/kit)\n              hint: git -C <dir> remote set-url \
                 origin <url> (each under --verbose)\nheld          refresh kit (origin drift)\n"
            ),
            "{args:?}: {text}"
        );
    }
    // the entry's block: the hold, and the command that fixes it
    let text = stdout(&repos(&ws, &ws.root(), &["status", "kit", "--verbose"]));
    assert!(
        text.starts_with(
            "kit  reference · third-party · leave HEAD · refresh held (origin drift)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "git -C {} remote set-url origin https://github.com/them/kit",
            kit.display()
        )),
        "{text}"
    );
    let report = parse(&repos(&ws, &ws.root(), &["sync", "kit", "--json"]));
    assert_eq!(
        report["status"]["entries"][0]["refresh"],
        serde_json::json!({"kind": "held", "by": "entry"})
    );
    assert_eq!(report["entries"][0]["fetch"]["kind"], "not_fetched");
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert_eq!(ws.refs(&kit), before);
}

/// A third-party reference whose `origin` is the repo over SSH: the refresh
/// is held with its own note, never fetched, and the run exits 0 — and so
/// is one an `insteadOf` rewrites to another repo over HTTPS, worded so.
#[test]
fn a_reference_with_an_ssh_origin_is_never_refreshed() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party_over_https("lib", "lib", &[]);
    ws.git(
        &lib,
        &["remote", "set-url", "origin", "git@github.com:them/lib"],
    );
    ws.upstream_commit("lib", "main");
    ws.serve_https();
    ws.write_registry();
    let before = ws.refs(&lib);

    for args in [
        &["sync", "lib"][..],
        &["sync", "--references"],
        &["status", "lib"],
        &["status", "--references", "--fetch"],
    ] {
        let out = repos(&ws, &ws.root(), args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        let text = stdout(&out);
        assert!(
            text.starts_with(
                "needs human   lib (refresh would fetch from git@github.com:them/lib, not over \
                 HTTPS)\nheld          refresh lib (origin not HTTPS)\n"
            ),
            "{args:?}: {text}"
        );
    }
    // the entry's block: the fix
    let text = stdout(&repos(&ws, &ws.root(), &["status", "lib", "--verbose"]));
    assert!(
        text.contains(&format!(
            "a reference is fetched only over HTTPS, from https://github.com/them/lib: git -C {} \
             remote set-url origin https://github.com/them/lib",
            lib.display()
        )),
        "{text}"
    );
    // an `insteadOf` rewrite makes it: the rewrite named, no set-url
    ws.git(
        &lib,
        &["remote", "set-url", "origin", "https://github.com/them/lib"],
    );
    ws.git(
        &lib,
        &[
            "config",
            "url.git@github.com:.insteadOf",
            "https://github.com/",
        ],
    );
    let text = stdout(&repos(&ws, &ws.root(), &["status", "lib", "--verbose"]));
    assert!(
        text.contains("a url.*.insteadOf rewrite makes it"),
        "{text}"
    );
    assert!(!text.contains("set-url"), "{text}");
    let report = parse(&repos(&ws, &ws.root(), &["sync", "lib", "--json"]));
    assert_eq!(
        report["status"]["entries"][0]["refresh"],
        serde_json::json!({"kind": "held", "by": "origin_not_https"})
    );
    assert_eq!(
        report["status"]["entries"][0]["needs_human"],
        serde_json::json!([{
            "kind": "origin_not_https",
            "fetch_url": "git@github.com:them/lib",
            "expected": "https://github.com/them/lib",
            "fix": null,
        }])
    );
    assert_eq!(report["entries"][0]["fetch"]["kind"], "not_fetched");
    // a rewrite to another repo over HTTPS: the repo named, not the transport
    ws.git(
        &lib,
        &["config", "--unset", "url.git@github.com:.insteadOf"],
    );
    ws.git(
        &lib,
        &[
            "config",
            "url.https://github.com/other/.insteadOf",
            "https://github.com/them/",
        ],
    );
    let out = repos(&ws, &ws.root(), &["sync", "lib"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(
            "needs human   lib (refresh would fetch from https://github.com/other/lib, not \
             https://github.com/them/lib)\nheld          refresh lib (origin elsewhere)\n"
        ),
        "{text}"
    );
    let text = stdout(&repos(&ws, &ws.root(), &["status", "lib", "--verbose"]));
    assert!(
        text.starts_with(
            "lib  reference · third-party · leave HEAD · refresh held (origin elsewhere)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("a url.*.insteadOf rewrite makes it"),
        "{text}"
    );
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert_eq!(ws.refs(&lib), before);
}

#[test]
fn references_with_targets_is_a_usage_error() {
    let ws = references_workspace();
    for command in ["status", "sync"] {
        let out = repos(&ws, &ws.root(), &[command, "--references", "lib"]);
        assert_eq!(out.status.code(), Some(2), "{command}");
        assert!(stdout(&out).is_empty(), "{command}: {}", stdout(&out));
        assert!(
            stderr(&out).starts_with("error: --references takes no targets\nhint: "),
            "{command}: {}",
            stderr(&out)
        );
        let out = repos(&ws, &ws.root(), &[command, "lib", "--references", "--json"]);
        assert_eq!(out.status.code(), Some(2), "{command}");
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["error"]["kind"], "references_with_targets", "{doc}");
    }
    // nothing was fetched
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
}
