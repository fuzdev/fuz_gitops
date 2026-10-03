//! Helpers shared by the rebase tests (`sync_rebase*`, `push_rebase*`): a
//! clone whose registry branch diverged from origin's, the diverged shapes
//! that aren't the tool's to rebase, what a run must leave untouched, and
//! what a replay must have made. Each fixture but `diverge` writes the
//! registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fuz_repos::push::PushRun;
use fuz_repos::report::{BranchOutcome, BranchSyncHold, PushOutcome, RebasePush, Rebased};
use fuz_repos::state::SyncAction;
use fuz_repos::sync::SyncRun;

use super::push::{only, remote_refs, with};
use super::sync::outcome;
use super::{FixtureWorkspace, ORIGIN_HEAD, TRACKING, files, write, write_executable};

/// The rebase `diverged`'s `main` reads as: `ahead` local-only commits
/// replayed onto `behind` upstream ones.
pub const fn rebase(ahead: u32, behind: u32) -> SyncAction {
    SyncAction::Rebase { ahead, behind }
}

/// `diverged`'s rebase, held by `by` as sync reports it: nothing moved.
pub const fn rebase_held(by: BranchSyncHold) -> BranchOutcome {
    BranchOutcome::Held {
        action: rebase(2, 1),
        by,
    }
}

/// A sync run's `Rebased` outcome for `app`'s `main`: `(from, to, onto,
/// push)`.
pub fn sync_rebased(run: &SyncRun) -> (&str, &str, &str, &RebasePush) {
    match outcome(run, "app", "main") {
        BranchOutcome::Rebased(Rebased {
            from,
            to,
            onto,
            push,
        }) => (from, to, onto, push),
        other => panic!("not rebased: {other:?}"),
    }
}

/// A push run's one target's `Rebased` outcome, of `main`: `(from, to,
/// onto, push)`.
pub fn push_rebased(run: &PushRun) -> (&str, &str, &str, &RebasePush) {
    match only(run) {
        (
            Some("main"),
            PushOutcome::Rebased(Rebased {
                from,
                to,
                onto,
                push,
            }),
        ) => (from, to, onto, push),
        other => panic!("not rebased: {other:?}"),
    }
}

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
    /// The branch's tip: its newest local-only commit.
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

/// The files a checkout of `diverged`'s replayed tip holds: both sides'.
pub const REBASED_FILES: [&str; 4] = ["README", "local-1.txt", "local-2.txt", "upstream-main.txt"];

/// Each kind of uncommitted change that holds a rebase (`dirty`).
pub const DIRT: [&str; 3] = ["untracked", "unstaged", "staged"];

/// Leaves `app`'s checkout dirty by `dirt`, one of `DIRT`.
pub fn dirty(ws: &FixtureWorkspace, app: &Path, dirt: &str) {
    match dirt {
        "untracked" => write(app, "scratch.txt", "mine\n"),
        "unstaged" => write(app, "README", "edited\n"),
        "staged" => {
            write(app, "README", "edited\n");
            ws.git(app, &["add", "README"]);
        }
        _ => panic!("no dirt {dirt}"),
    }
    assert_ne!(ws.git_raw(app, &["status", "--porcelain"]), "", "{dirt}");
}

/// A clone whose `main` and origin's each add `upstream-main.txt`, with
/// content of their own: a replay conflicts.
pub struct Conflicting {
    pub app: PathBuf,
    /// The one local commit.
    pub local: String,
    /// Origin's tip, fetched.
    pub upstream: String,
}

/// `name`, owned, its `main` a commit ahead and one behind that conflict;
/// fetched, clean.
pub fn conflicting(ws: &mut FixtureWorkspace, name: &str) -> Conflicting {
    let app = ws.owned_repo(name, &[]);
    let local = ws.commit(&app, "upstream-main");
    let upstream = ws.upstream_commit(name, "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    assert_ne!(
        ws.git(&app, &["rev-parse", "main:upstream-main.txt"]),
        ws.git(&app, &["rev-parse", "origin/main:upstream-main.txt"])
    );
    ws.assert_clean(&app);
    ws.write_registry();
    Conflicting {
        app,
        local,
        upstream,
    }
}

/// `app` with a merge among `main`'s local-only commits; fetched, clean.
pub fn merge_in_range(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    ws.git(&app, &["switch", "-q", "-c", "topic"]);
    ws.commit(&app, "topic");
    ws.git(&app, &["switch", "-q", "main"]);
    ws.commit(&app, "local");
    ws.git(
        &app,
        &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
    );
    ws.git(&app, &["branch", "-q", "-D", "topic"]);
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 3, behind 1]");
    ws.assert_count(&app, &["--merges", "origin/main..main"], 1);
    ws.assert_clean(&app);
    ws.write_registry();
    app
}

/// `app` diverged, a tag `v1` — annotated or not — on its first local-only
/// commit (a release made here, its push refused), and a tag on a commit
/// origin has, which holds nothing.
pub fn tag_in_range(ws: &mut FixtureWorkspace, annotated: bool) -> Diverged {
    let app = ws.owned_repo("app", &[]);
    let d = diverge(ws, app);
    if annotated {
        ws.git(&d.app, &["tag", "-a", "-m", "v1", "v1", &d.local[0]]);
    } else {
        ws.git(&d.app, &["tag", "v1", &d.local[0]]);
    }
    ws.git(&d.app, &["tag", "base", "origin/main~1"]);
    assert_eq!(ws.git(&d.app, &["rev-parse", "v1^{commit}"]), d.local[0]);
    ws.write_registry();
    d
}

/// `app` whose `main` is ahead by a commit origin's `feat` holds — `feat`
/// pushed, then merged here by fast-forward — and, with `own_commit_too`,
/// one of its own: the clone and `feat`'s commit.
pub fn published_in_range(ws: &mut FixtureWorkspace, own_commit_too: bool) -> (PathBuf, String) {
    ws.remote("app", &[]);
    let feat = ws.upstream_commit("app", "feat");
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    ws.git(&app, &["merge", "-q", "--ff-only", "origin/feat"]);
    assert_eq!(ws.git(&app, &["rev-parse", "main"]), feat);
    let (ahead, own) = if own_commit_too {
        ws.commit(&app, "local");
        (2, 1)
    } else {
        (1, 0)
    };
    ws.upstream_commit("app", "main");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", &format!("[ahead {ahead}, behind 1]"));
    ws.assert_count(&app, &["main", "--not", "--remotes"], own);
    assert_eq!(ws.git(&app, &["rev-parse", "origin/feat"]), feat);
    ws.assert_clean(&app);
    ws.write_registry();
    (app, feat)
}

/// `app`, archived, its `main` diverged.
pub fn archived(ws: &mut FixtureWorkspace) -> Diverged {
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "archived = true");
    let app = ws.clone_owned("app", "app", &[]);
    let d = diverge(ws, app);
    ws.write_registry();
    d
}

/// `app` a commit ahead and one behind, origin's commit tracking a path
/// the clone ignores and holds a file at: status reads the checkout clean,
/// and the move to the replayed commits must not replace the file. The
/// clone and its local commit.
pub fn ignored_file_upstream(ws: &mut FixtureWorkspace) -> (PathBuf, String) {
    ws.remote("app", &[(".gitignore", "secret.env\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    let local = ws.commit(&app, "local");
    let up = ws.upstream("app");
    write(&up, "secret.env", "tracked\n");
    ws.git(&up, &["add", "-f", "secret.env"]);
    ws.git(&up, &["commit", "-q", "-m", "track it"]);
    publish(ws, "app");
    write(&app, "secret.env", "mine\n");
    ws.git(&app, &["fetch", "-q", "origin"]);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    ws.assert_clean(&app);
    ws.write_registry();
    (app, local)
}

/// What `secret.env` is refused with (`ignored_file_upstream`): git's
/// message, the file it lists on the line after joined onto it, as the
/// report gives it.
pub const IGNORED_FILE_REFUSED: &str = "error: The following untracked working tree files would \
                                        be overwritten by checkout: secret.env";

/// A bare remote `name` whose pre-receive hook refuses every push, as a
/// protected branch would; the hook's path, to lift the refusal.
pub fn refuse_pushes(ws: &FixtureWorkspace, name: &str) -> PathBuf {
    write_executable(
        &ws.bare(name),
        "hooks/pre-receive",
        "#!/bin/sh\necho 'error: GH006: Protected branch update failed.' >&2\nexit 1\n",
    );
    ws.bare(name).join("hooks/pre-receive")
}

/// The clone's refs `before`, once its `main` was rebased to `to` and
/// pushed: the branch, and the remote-tracking ref the push recorded
/// (origin's `HEAD` read through it) — nothing else.
pub fn refs_rebased(before: &BTreeMap<String, String>, to: &str) -> BTreeMap<String, String> {
    with(
        before,
        &[("refs/heads/main", to), (TRACKING, to), (ORIGIN_HEAD, to)],
    )
}

/// The repo's loose objects, as `count-objects` counts them: a replay
/// writes some, so the same count after a run says none ran.
pub fn loose_objects(ws: &FixtureWorkspace, app: &Path) -> String {
    ws.git(app, &["count-objects", "-v"])
        .lines()
        .find(|l| l.starts_with("count:"))
        .unwrap()
        .to_owned()
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

/// `app`'s state now, and its remote's, to compare after a run that must
/// leave both alone.
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
/// tip, `to`: the same changes, subjects, and authors, a linear chain on
/// the fetched tip, new commits.
pub fn assert_replayed(ws: &FixtureWorkspace, d: &Diverged, to: &str) {
    let app = &d.app;
    assert_eq!(ws.git(app, &["rev-parse", "main"]), to);
    assert_eq!(ws.git(app, &["rev-parse", "main~2"]), d.upstream);
    ws.assert_count(app, &["--merges", &format!("{}..main", d.upstream)], 0);
    let said = |rev: &str| ws.git(app, &["log", "-1", "--format=%s %an %ae %at", rev]);
    // each commit's own change, as a patch: the fixture's commits add
    // files no other commit touches, so a faithful replay's diff is the
    // original's, byte for byte
    let change = |rev: &str| ws.git(app, &["diff-tree", "-p", "--no-commit-id", rev]);
    for (replayed, original) in [("main~1", &d.local[0]), ("main", &d.local[1])] {
        assert_eq!(said(replayed), said(original), "{replayed}");
        assert_eq!(change(replayed), change(original), "{replayed}");
    }
    assert_ne!(to, d.tip());
    // the originals stay reachable, by the branch's reflog
    assert_eq!(ws.git(app, &["rev-parse", "main@{1}"]), d.tip());
}
