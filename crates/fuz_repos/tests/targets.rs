//! Target resolution against a fixture workspace: a key, a dir name, and a
//! path inside a checkout — `.` from a subdir and from a linked worktree
//! outside the workspace included.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use fuz_repos::discover::resolve_targets;
use fuz_repos::error::Error;
use support::FixtureWorkspace;

/// A workspace with `app` (dir `app-dir`) and `lib`.
fn workspace() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("src/main.rs", "fn main() {}\n")]);
    ws.remote("lib", &[]);
    ws.declare_repo("app", "app", "dir = \"app-dir\"");
    ws.declare_repo("lib", "lib", "");
    ws.clone_owned("app-dir", "app", &[]);
    ws.clone_owned("lib", "lib", &[]);
    ws
}

/// The keys `targets` select, resolved from `cwd`.
fn keys(ws: &FixtureWorkspace, cwd: &std::path::Path, targets: &[&str]) -> Vec<String> {
    let targets: Vec<String> = targets.iter().map(|&t| t.to_owned()).collect();
    resolve_targets(&ws.entries(), &ws.root(), cwd, &targets, &ws.runner())
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect()
}

#[test]
fn no_targets_select_every_entry() {
    let ws = workspace();
    assert_eq!(keys(&ws, &ws.root(), &[]), ["app", "lib"]);
}

#[test]
fn a_key_or_a_dir_name() {
    let ws = workspace();
    assert_eq!(keys(&ws, &ws.root(), &["app"]), ["app"]);
    assert_eq!(keys(&ws, &ws.root(), &["app-dir"]), ["app"]);
    // registry order, deduplicated
    assert_eq!(
        keys(&ws, &ws.root(), &["lib", "app-dir", "app"]),
        ["app", "lib"]
    );
}

#[test]
fn a_path_inside_a_checkout() {
    let ws = workspace();
    let src = ws.dir("app-dir").join("src");
    assert!(src.is_dir());
    // relative to the cwd
    assert_eq!(keys(&ws, &ws.root(), &["app-dir/src"]), ["app"]);
    assert_eq!(keys(&ws, &ws.root(), &["./lib"]), ["lib"]);
    // absolute
    assert_eq!(keys(&ws, &ws.root(), &[src.to_str().unwrap()]), ["app"]);
    // `.` from a subdir, and `..` back to the checkout
    assert_eq!(keys(&ws, &src, &["."]), ["app"]);
    assert_eq!(keys(&ws, &src, &[".."]), ["app"]);
}

#[test]
fn dot_in_a_linked_worktree_outside_the_workspace() {
    let ws = workspace();
    let elsewhere = ws.outside("app-feature");
    ws.git(
        &ws.dir("app-dir"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            elsewhere.to_str().unwrap(),
        ],
    );
    assert!(elsewhere.join(".git").is_file());
    assert!(!elsewhere.starts_with(ws.root()));
    assert_eq!(keys(&ws, &elsewhere, &["."]), ["app"]);
}

#[test]
fn an_unknown_target_is_an_error() {
    let ws = workspace();
    let not_a_repo = ws.outside("plain");
    std::fs::create_dir(&not_a_repo).unwrap();
    for target in ["nope", "plain", "../plain", not_a_repo.to_str().unwrap()] {
        let e = resolve_targets(
            &ws.entries(),
            &ws.root(),
            &ws.root(),
            &[target.to_owned()],
            &ws.runner(),
        )
        .unwrap_err();
        assert!(
            matches!(&e, Error::UnknownEntry { name, .. } if name == target),
            "{target}: {e}"
        );
        assert_eq!(e.exit_code(), 2);
    }
}

#[test]
fn an_unregistered_repo_in_the_workspace_is_unknown() {
    let ws = workspace();
    ws.remote("stray", &[]);
    ws.clone_owned("stray", "stray", &[]);
    let e = resolve_targets(
        &ws.entries(),
        &ws.root(),
        &ws.dir("stray"),
        &[".".to_owned()],
        &ws.runner(),
    )
    .unwrap_err();
    assert!(matches!(e, Error::UnknownEntry { .. }), "{e}");
}
