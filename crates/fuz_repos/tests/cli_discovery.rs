//! Registry discovery and workspace roots through the binary: `--registry` /
//! `--root`, registries kept in a repo, and roots found in checkouts. Run under
//! the same hermetic environment as the fixtures.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use support::cli::{error_doc, parse, repos, stderr, stdout, workspace};
use support::{FixtureWorkspace, THIRD_PARTY};

#[test]
fn a_registry_outside_the_workspace_with_root() {
    let ws = workspace();
    let meta = ws.outside("meta");
    std::fs::create_dir(&meta).unwrap();
    let registry = meta.join("repos.toml");
    std::fs::rename(ws.root().join("repos.toml"), &registry).unwrap();

    // without --root, the registry's dir is the root and every entry is missing
    let report = parse(&repos(
        &ws,
        &ws.root(),
        &["--registry", registry.to_str().unwrap(), "status", "--json"],
    ));
    assert_eq!(report["workspace"], meta.to_str().unwrap());
    assert_eq!(report["entries"][0]["presence"]["kind"], "missing");

    let report = parse(&repos(
        &ws,
        &meta,
        &[
            "--registry",
            "repos.toml",
            "--root",
            ws.root().to_str().unwrap(),
            "status",
            "--json",
        ],
    ));
    assert_eq!(report["workspace"], ws.root().to_str().unwrap());
    assert_eq!(report["registry"], registry.to_str().unwrap());
    assert_eq!(report["entries"][0]["presence"]["kind"], "present");
}

#[test]
fn the_discovery_fallback_is_for_linked_worktrees_only() {
    let ws = workspace();
    // a checkout whose git dir lives in a dir holding a registry: the git
    // dir's parent is no checkout, and discovery doesn't search it
    let elsewhere = ws.outside("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::copy(ws.root().join("repos.toml"), elsewhere.join("repos.toml")).unwrap();
    let home = ws.outside("home2");
    let git_dir = elsewhere.join("dot.git");
    ws.git(
        ws.base(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", git_dir.display()),
            home.to_str().unwrap(),
        ],
    );
    ws.git(&home, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let proj = home.join("proj");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(
        ws.git(
            &proj,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        git_dir.to_str().unwrap()
    );
    let error = error_doc(&repos(&ws, &proj, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // nor from a linked worktree of it: its common dir isn't a `.git` in a
    // main checkout
    let wt = ws.outside("home2-wt");
    ws.add_worktree(&home, &wt, &["-b", "wt"]);
    let error = error_doc(&repos(&ws, &wt, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // nor when the separate git dir is itself named `.git`: it isn't a
    // linked worktree, so no fallback, though the common dir's parent looks
    // like a checkout
    let elsewhere3 = ws.outside("elsewhere3");
    std::fs::create_dir(&elsewhere3).unwrap();
    std::fs::copy(ws.root().join("repos.toml"), elsewhere3.join("repos.toml")).unwrap();
    let home3 = ws.outside("home3");
    ws.git(
        ws.base(),
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", elsewhere3.join(".git").display()),
            home3.to_str().unwrap(),
        ],
    );
    let proj3 = home3.join("proj");
    std::fs::create_dir(&proj3).unwrap();
    let error = error_doc(&repos(&ws, &proj3, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "registry_not_found");
    // control: from the registry's own dir it's found
    let out = repos(&ws, &elsewhere, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

/// `app` ahead by one, and the registry kept in `meta`, linked at the root
/// — committed there when `tracked`, else ignored.
fn registry_repo_workspace(tracked: bool) -> (FixtureWorkspace, PathBuf) {
    registry_repo_workspace_in(tracked, "")
}

/// `registry_repo_workspace` with the registry in `meta/<sub>`.
fn registry_repo_workspace_in(tracked: bool, sub: &str) -> (FixtureWorkspace, PathBuf) {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let meta = ws.owned_repo("meta", &[]);
    ws.commit(&app, "local");
    // Claude Code's worktrees dir too, as a user's global excludes would
    let exclude = if tracked {
        ".claude/\n"
    } else {
        ".claude/\nrepos.toml\n"
    };
    support::write(&meta, ".git/info/exclude", exclude);
    let registry = ws.write_registry_in(&meta.join(sub));
    if tracked {
        ws.git(&meta, &["add", "-A"]);
        ws.git(&meta, &["commit", "-q", "-m", "registry"]);
        ws.assert_track(&meta, "main", "[ahead 1]");
    }
    ws.assert_clean(&meta);
    assert_eq!(registry, meta.join(sub).join("repos.toml"));
    (ws, meta)
}

/// The workspace, the registry, and the entries' keys a `status --json`
/// run with `args` from `cwd` reports.
fn discovered(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> (String, String, Vec<String>) {
    let report = parse(&repos(ws, cwd, args));
    let keys = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap().to_owned())
        .collect();
    (
        report["workspace"].as_str().unwrap().to_owned(),
        report["registry"].as_str().unwrap().to_owned(),
        keys,
    )
}

#[test]
fn a_registry_kept_in_a_repo_roots_at_its_link() {
    let (ws, meta) = registry_repo_workspace(false);
    let root = ws.root().to_str().unwrap().to_owned();
    let link = ws.root().join("repos.toml").to_str().unwrap().to_owned();
    let deeper = meta.join("src/deeper");
    std::fs::create_dir_all(&deeper).unwrap();

    // from inside the repo holding it, the walk goes on to the link
    for cwd in [&meta, &deeper] {
        let report = parse(&repos(&ws, cwd, &["status", "--json"]));
        assert_eq!(report["workspace"], root.as_str());
        assert_eq!(report["registry"], link.as_str());
        let presence: Vec<&str> = report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["presence"]["kind"].as_str().unwrap())
            .collect();
        assert_eq!(presence, ["present", "present"]);
        assert_eq!(
            discovered(&ws, cwd, &["status", "--json", "."]),
            (root.clone(), link.clone(), vec!["meta".to_owned()])
        );
    }
    let out = repos(&ws, &meta, &["status"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.lines()
            .last()
            .unwrap()
            .contains(&format!("      {link} · fetched ")),
        "{text}"
    );
    assert!(text.contains("sync would    push app +1\n"), "{text}");

    // an explicit registry is its own dir's, link or not
    let (workspace, registry, _) = discovered(
        &ws,
        &meta,
        &["--registry", "repos.toml", "status", "--json"],
    );
    assert_eq!(workspace, meta.to_str().unwrap());
    assert_eq!(registry, meta.join("repos.toml").to_str().unwrap());
}

#[test]
fn a_linked_worktree_of_the_repo_keeping_the_registry_roots_at_its_link() {
    let root = |ws: &FixtureWorkspace| ws.root().to_str().unwrap().to_owned();
    let link = |ws: &FixtureWorkspace| ws.root().join("repos.toml").to_str().unwrap().to_owned();
    let meta_only = vec!["meta".to_owned()];

    // committed: each worktree has its own copy, found first, and the main
    // checkout's is the one linked
    let (ws, meta) = registry_repo_workspace(true);
    let outside = ws.outside("meta-wt");
    ws.add_worktree(&meta, &outside, &["-b", "wt"]);
    let inside = meta.join(".claude/worktrees/agent");
    ws.add_worktree(&meta, &inside, &["-b", "agent"]);
    ws.assert_clean(&meta);
    let deeper = inside.join("src");
    std::fs::create_dir_all(&deeper).unwrap();
    for wt in [&outside, &inside] {
        let own = std::fs::symlink_metadata(wt.join("repos.toml")).unwrap();
        let main = std::fs::metadata(meta.join("repos.toml")).unwrap();
        assert!(own.is_file() && !own.file_type().is_symlink());
        assert_ne!(own.ino(), main.ino());
    }
    for cwd in [&outside, &inside, &deeper] {
        assert_eq!(
            discovered(&ws, cwd, &["status", "--json", "."]),
            (root(&ws), link(&ws), meta_only.clone()),
            "from {}",
            cwd.display()
        );
    }
    // with no link at the root, a worktree's own copy would root it in a
    // checkout of `meta`: refused
    std::fs::remove_file(ws.root().join("repos.toml")).unwrap();
    let error = error_doc(&repos(&ws, &outside, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "root_in_entry");
    assert_eq!(error["key"], "meta");

    // ignored: a worktree has none, and the walk from the main checkout
    // goes on to the link the same way
    let (ws, meta) = registry_repo_workspace(false);
    let outside = ws.outside("meta-wt");
    ws.add_worktree(&meta, &outside, &["-b", "wt"]);
    assert!(!outside.join("repos.toml").exists());
    assert_eq!(
        discovered(&ws, &outside, &["status", "--json", "."]),
        (root(&ws), link(&ws), meta_only)
    );
}

#[test]
fn a_registry_below_a_repos_top_maps_to_the_same_place_in_its_main_checkout() {
    let (ws, meta) = registry_repo_workspace_in(true, "cfg");
    let link = ws.root().join("repos.toml");
    assert_eq!(link.canonicalize().unwrap(), meta.join("cfg/repos.toml"));
    let wt = ws.outside("meta-wt");
    ws.add_worktree(&meta, &wt, &["-b", "wt"]);
    assert!(wt.join("cfg/repos.toml").is_file());
    assert!(!wt.join("repos.toml").exists());
    let deep = wt.join("cfg/deep");
    std::fs::create_dir(&deep).unwrap();

    let expected = (
        ws.root().to_str().unwrap().to_owned(),
        link.to_str().unwrap().to_owned(),
        vec!["meta".to_owned()],
    );
    // the worktree's own copy at `cfg/`, then the main checkout's there;
    // from the worktree's top, the walk from the main checkout
    for cwd in [&deep, &wt.join("cfg"), &wt, &meta.join("cfg")] {
        assert_eq!(
            discovered(&ws, cwd, &["status", "--json", "."]),
            expected,
            "from {}",
            cwd.display()
        );
    }
}

/// The registry committed in `meta`, never linked at the root: a fresh
/// machine's layout before the link is made.
fn unlinked_registry_workspace() -> (FixtureWorkspace, PathBuf) {
    let (ws, meta) = registry_repo_workspace(true);
    std::fs::remove_file(ws.root().join("repos.toml")).unwrap();
    assert!(!ws.root().join("repos.toml").exists());
    assert!(meta.join("repos.toml").is_file());
    (ws, meta)
}

#[test]
fn a_root_found_in_an_entrys_checkout_is_refused() {
    let (ws, meta) = unlinked_registry_workspace();
    let deeper = meta.join("src");
    std::fs::create_dir(&deeper).unwrap();
    // a rewrite sends origin's fetches to the local bare remote
    let rewrite = format!("url.file://{}.insteadOf", ws.bare("meta").display());
    ws.git(&meta, &["config", &rewrite, &support::owned_origin("meta")]);

    for cwd in [&meta, &deeper] {
        let out = repos(&ws, cwd, &["status", "--json"]);
        let error = error_doc(&out, 2);
        assert_eq!(error["kind"], "root_in_entry");
        assert_eq!(error["key"], "meta");
        assert_eq!(
            error["message"],
            format!(
                "the registry found at {}/repos.toml would root the workspace at {}, a \
                 checkout of entry `meta`",
                meta.display(),
                meta.display()
            )
        );
        // and in text, and for sync, which would clone into it
        let out = repos(&ws, cwd, &["status"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(
            stderr(&out).contains("hint: run from the workspace root"),
            "{}",
            stderr(&out)
        );
        let out = repos(&ws, cwd, &["sync", "--json"]);
        assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
        let doc: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(doc["error"]["kind"], "root_in_entry");
    }
    assert!(!meta.join("app").exists());
    // the fixture's origin names `meta` as configured only: git resolves
    // it to the local bare remote
    assert_eq!(
        ws.git(&meta, &["config", "remote.origin.url"]),
        support::owned_origin("meta")
    );
    assert!(
        ws.git(&meta, &["remote", "get-url", "origin"])
            .starts_with("file://")
    );
    // and one spelled through an alias names it only as resolved
    ws.git(&meta, &["remote", "set-url", "origin", "gh:me/meta"]);
    ws.git(&meta, &["config", "url.git@github.com:.insteadOf", "gh:"]);
    assert_eq!(
        ws.git(&meta, &["config", "remote.origin.url"]),
        "gh:me/meta"
    );
    assert_eq!(
        ws.git(&meta, &["remote", "get-url", "origin"]),
        support::owned_origin("meta")
    );
    let error = error_doc(&repos(&ws, &meta, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "root_in_entry");
    assert_eq!(error["key"], "meta");
    ws.git(
        &meta,
        &[
            "remote",
            "set-url",
            "origin",
            &support::owned_origin("meta"),
        ],
    );
    // a bare repo's worktree: no main checkout to defer to
    let bare = ws.outside("meta.git");
    ws.git(
        ws.base(),
        &[
            "clone",
            "-q",
            "--bare",
            meta.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    ws.git(
        &bare,
        &[
            "remote",
            "set-url",
            "origin",
            &support::owned_origin("meta"),
        ],
    );
    let wt = ws.outside("meta-bare-wt");
    ws.add_worktree(&bare, &wt, &["main"]);
    assert!(wt.join("repos.toml").is_file());
    let error = error_doc(&repos(&ws, &wt, &["status", "--json"]), 2);
    assert_eq!(error["kind"], "root_in_entry");
    assert_eq!(error["key"], "meta");

    // named, it runs
    let root = ws.root().to_str().unwrap().to_owned();
    let (workspace, _, _) = discovered(&ws, &meta, &["--root", &root, "status", "--json"]);
    assert_eq!(workspace, root);
    let (workspace, _, _) = discovered(
        &ws,
        &meta,
        &["--registry", "repos.toml", "status", "--json"],
    );
    assert_eq!(workspace, meta.to_str().unwrap());
    // and linked at the root, it roots there
    std::os::unix::fs::symlink(meta.join("repos.toml"), ws.root().join("repos.toml")).unwrap();
    let (workspace, _, _) = discovered(&ws, &meta, &["status", "--json"]);
    assert_eq!(workspace, root);
}

#[test]
fn a_root_in_a_checkout_of_no_entry_runs() {
    let ws = workspace();
    let root = ws.root().to_str().unwrap().to_owned();
    let app = ws.dir("app");
    // a dotfiles-style repo further out, holding the workspace
    ws.git(ws.base(), &["init", "-q"]);
    ws.git(
        ws.base(),
        &["remote", "add", "origin", "git@github.com:me/dotfiles"],
    );
    let (workspace, _, _) = discovered(&ws, &app, &["status", "--json"]);
    assert_eq!(workspace, root);
    // the workspace itself a repo its registry doesn't list
    ws.git(&ws.root(), &["init", "-q"]);
    ws.git(
        &ws.root(),
        &["remote", "add", "origin", "git@github.com:me/workspace"],
    );
    let (workspace, _, _) = discovered(&ws, &app, &["status", "--json"]);
    assert_eq!(workspace, root);
    // one whose origin names an entry is that entry's checkout — a
    // reference's as much as a repo's
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    ws.declare_reference("spec", THIRD_PARTY, "spec", "");
    ws.write_registry();
    ws.git(&ws.root(), &["init", "-q"]);
    ws.git(
        &ws.root(),
        &[
            "remote",
            "add",
            "origin",
            &support::third_party_origin("spec"),
        ],
    );
    let error = error_doc(&repos(&ws, &ws.dir("app"), &["status", "--json"]), 2);
    assert_eq!(error["kind"], "root_in_entry");
    assert_eq!(error["key"], "spec");
}
