//! Helpers shared by the rebase tests (`sync_rebase`, `push_rebase`): a
//! clone whose registry branch diverged from origin's, what a run must
//! leave untouched, and what a replay must have made.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::push::remote_refs;
use super::{FixtureWorkspace, files};

pub const TRACKING: &str = "refs/remotes/origin/main";
/// Origin's `HEAD`, a symbolic ref to `TRACKING`: `refs` reads it through.
pub const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

/// Moves the bare remote's `main` to the upstream author's, by a fetch into
/// it.
pub fn publish(ws: &FixtureWorkspace, name: &str) {
    let up = ws.upstream(name);
    ws.git(
        &ws.bare(name),
        &[
            "fetch",
            "-q",
            up.to_str().unwrap(),
            "+refs/heads/main:refs/heads/main",
        ],
    );
}

/// A clone of `app` whose `main` diverged from origin's.
pub struct Diverged {
    pub app: PathBuf,
    /// The local-only commits, oldest first.
    pub local: Vec<String>,
    /// Origin's tip, fetched.
    pub upstream: String,
}

impl Diverged {
    pub fn tip(&self) -> &str {
        self.local.last().unwrap()
    }
}

/// `app`'s clone with two commits on `main` and one more on origin's,
/// fetched, so it reads diverged before the tool looks; the entry's table
/// is the caller's to declare.
pub fn diverge(ws: &FixtureWorkspace, app: PathBuf) -> Diverged {
    let local = vec![ws.commit(&app, "local-1"), ws.commit(&app, "local-2")];
    let upstream = ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    assert_eq!(ws.git(&app, &["rev-parse", TRACKING]), upstream);
    ws.assert_track(&app, "main", "[ahead 2, behind 1]");
    ws.assert_count(&app, &["--merges", "origin/main..main"], 0);
    ws.assert_head(&app, Some("main"));
    ws.assert_clean(&app);
    Diverged {
        app,
        local,
        upstream,
    }
}

/// `app`, owned, its `main` — the registry's branch — diverged, checked
/// out and clean.
pub fn diverged(ws: &mut FixtureWorkspace) -> Diverged {
    let app = ws.owned_repo("app", &[]);
    let d = diverge(ws, app);
    ws.write_registry();
    d
}

/// What a run must leave untouched: the clone's refs, index, and files,
/// and the remote's refs.
#[derive(Debug, PartialEq, Eq)]
pub struct Untouched {
    refs: BTreeMap<String, String>,
    index: Vec<u8>,
    staged: String,
    files: Vec<(String, Vec<u8>)>,
    remote: BTreeMap<String, String>,
}

pub fn untouched(ws: &FixtureWorkspace, app: &Path) -> Untouched {
    Untouched {
        refs: ws.refs(app),
        index: std::fs::read(app.join(".git/index")).unwrap(),
        staged: ws.git_raw(app, &["ls-files", "--stage"]),
        files: files(app)
            .into_iter()
            .map(|f| {
                let bytes = std::fs::read(app.join(&f)).unwrap();
                (f, bytes)
            })
            .collect(),
        remote: remote_refs(ws, "app"),
    }
}

/// Asserts `main` in `app` is `d`'s local commits replayed onto origin's
/// tip, `to`: the same subjects and authors, a linear chain on the fetched
/// tip, new commits.
pub fn assert_replayed(ws: &FixtureWorkspace, d: &Diverged, to: &str) {
    let app = &d.app;
    assert_eq!(ws.git(app, &["rev-parse", "main"]), to);
    assert_eq!(ws.git(app, &["rev-parse", "main~2"]), d.upstream);
    ws.assert_count(app, &["--merges", &format!("{}..main", d.upstream)], 0);
    let said = |rev: &str| ws.git(app, &["log", "-1", "--format=%s %an %ae %at", rev]);
    assert_eq!(said("main~1"), said(&d.local[0]));
    assert_eq!(said("main"), said(&d.local[1]));
    assert_ne!(to, d.tip());
    // the originals stay reachable, by the branch's reflog
    assert_eq!(ws.git(app, &["rev-parse", "main@{1}"]), d.tip());
}
