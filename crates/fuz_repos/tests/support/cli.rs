//! Helpers shared by the `cli_*` tests: running the `repos` binary over a
//! fixture workspace and reading its output.

use std::path::Path;
use std::process::Output;

use super::FixtureWorkspace;
use fuz_repos::STATUS_FORMAT_VERSION;
use serde_json::Value;

pub const REPOS: &str = env!("CARGO_BIN_EXE_repos");

pub fn repos(ws: &FixtureWorkspace, cwd: &Path, args: &[&str]) -> Output {
    ws.command(REPOS, cwd).args(args).output().unwrap()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// A workspace with `app` ahead by one and `gone` missing.
pub fn workspace() -> FixtureWorkspace {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.declare_repo("gone", "gone", "");
    ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    ws
}

pub fn parse(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap()
}

/// A fatal error's `--json` document: exactly `version` and `error`, the
/// message and hint the same ones stderr prints. Returns `error`.
pub fn error_doc(out: &Output, code: i32) -> Value {
    assert_eq!(out.status.code(), Some(code), "stderr: {}", stderr(out));
    let doc: Value = serde_json::from_str(&stdout(out))
        .map_err(|e| format!("{e}: stdout: {}", stdout(out)))
        .unwrap();
    let fields: Vec<&String> = doc.as_object().unwrap().keys().collect();
    assert_eq!(fields, ["error", "version"], "{doc}");
    assert_eq!(doc["version"], STATUS_FORMAT_VERSION);
    let error = doc["error"].clone();
    let err = stderr(out);
    let message = error["message"].as_str().unwrap();
    assert!(err.starts_with(&format!("error: {message}\n")), "{err}");
    match error["hint"].as_str() {
        Some(hint) => assert!(err.ends_with(&format!("\nhint: {hint}\n")), "{err}"),
        None => assert!(!err.contains("hint:"), "{err}"),
    }
    error
}
