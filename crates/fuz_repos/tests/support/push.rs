//! Helpers shared by the `push_*` tests: expected outcomes, the remote-side
//! refs, and the fixture repos.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::FixtureWorkspace;
use fuz_repos::push::PushRun;
use fuz_repos::report::PushOutcome;

/// The one target's outcome, and the branch it names.
pub fn only(run: &PushRun) -> (Option<&str>, &PushOutcome) {
    assert_eq!(run.pushes.len(), 1, "{:?}", run.pushes);
    let p = &run.pushes[0];
    (p.branch.as_deref(), &p.outcome)
}

pub fn pushed(from: &str, to: &str) -> PushOutcome {
    PushOutcome::Pushed {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// The remote's refs: every ref of `name`'s bare remote, tags included, and
/// its `HEAD`.
pub fn remote_refs(ws: &FixtureWorkspace, name: &str) -> BTreeMap<String, String> {
    ws.refs(&ws.bare(name))
}

/// `before` with `changes` applied.
pub fn with(
    before: &BTreeMap<String, String>,
    changes: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let mut refs = before.clone();
    for (r, oid) in changes {
        refs.insert((*r).to_owned(), (*oid).to_owned());
    }
    refs
}

/// The calls `ssh_push_log` holds — every push, or try at one — each as the
/// command it asked the host for (the whole line when it named no host).
pub fn pushes_served(ws: &FixtureWorkspace) -> Vec<String> {
    ws.ssh_push_log()
        .into_iter()
        .map(|l| match l.rsplit_once(" git@github.com ") {
            Some((_, command)) => command.to_owned(),
            None => l,
        })
        .collect()
}

/// `app`, owned, its `main` ahead of origin by one commit; returns the
/// clone and that commit.
pub fn ahead(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    let app = ws.owned_repo("app", &[]);
    let tip = ws.commit(&app, "local");
    ws.assert_track(&app, "main", "[ahead 1]");
    ws.write_registry();
    (app, tip)
}

/// `app` with `feat` on its remote, cloned, `feat` checked out tracking
/// origin's and a commit ahead of it; returns the clone and that commit.
pub fn feat_ahead(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    ws.remote("app", &[]);
    ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["switch", "-q", "feat"]);
    ws.assert_upstream(&app, "feat", "refs/remotes/origin/feat");
    let tip = ws.commit(&app, "local");
    ws.assert_track(&app, "feat", "[ahead 1]");
    ws.write_registry();
    (app, tip)
}

// --- --new-branch ---

/// `app` with a local `topic` checked out, no upstream, a commit on it;
/// returns the clone and that commit.
pub fn topic(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    let tip = ws.commit(&app, "topic");
    ws.assert_upstream(&app, "topic", "");
    ws.write_registry();
    (app, tip)
}

/// Asserts `topic`'s upstream is origin's `topic` as `git push -u` sets it,
/// tracking the commit `tip`, in sync.
pub fn assert_tracks_origin(ws: &FixtureWorkspace, app: &Path, branch: &str, tip: &str) {
    let key = |k: &str| format!("branch.{branch}.{k}");
    assert_eq!(
        ws.git(app, &["config", "--get-all", &key("remote")]),
        "origin"
    );
    assert_eq!(
        ws.git(app, &["config", "--get-all", &key("merge")]),
        format!("refs/heads/{branch}")
    );
    ws.assert_upstream(app, branch, &format!("refs/remotes/origin/{branch}"));
    assert_eq!(
        ws.git(
            app,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        tip
    );
    ws.assert_track(app, branch, "");
}
