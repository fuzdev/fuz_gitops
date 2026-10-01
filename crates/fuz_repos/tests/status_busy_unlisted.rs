//! Sessions working in git dirs git does not list: hand-made, partly linked,
//! and new-workdir git dirs.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};

use fuz_repos::classify::NeedsHuman;
use fuz_repos::report::Sessions;
use fuz_repos::sessions::{LiveSessions, SessionSource};
use fuz_repos::state::{BranchHold, Head, Verdict};
use support::busy::{MAX_GITFILE_BYTES, ahead_branch, app, held, push, session};
use support::{FixtureWorkspace, branch, find_entry, path};

/// `app` with `main` ahead in the primary and `other` ahead, checked out
/// nowhere; returns the primary.
fn app_with_other(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = app(ws);
    ws.commit(&app, "local");
    ahead_branch(ws, &app, "other");
    app
}

/// A session at `cwd` works through the unlisted git dir `git_dir`, whose
/// HEAD is `head`: `app`'s branches it may be on are held as busy unknown,
/// the rest act, and the reason names it.
fn assert_unlisted(
    ws: &FixtureWorkspace,
    cwd: &Path,
    git_dir: &Path,
    head: Option<Head>,
    held_ones: &[(&str, u32)],
    acting: &[(&str, u32)],
) {
    let s = session(9, cwd, SessionSource::SessionFile);
    let run = ws.status_live(&LiveSessions::Known(vec![s.clone()]));
    assert_eq!(run.sessions, Sessions::Available { unscoped: vec![] });
    let e = find_entry(&run.entries, "app");
    assert!(e.checkouts.iter().all(|c| c.busy.is_empty()));
    assert_eq!(
        e.needs_human,
        [NeedsHuman::UnlistedGitDir {
            git_dir: path(&git_dir.canonicalize().unwrap()),
            head,
            busy: vec![s],
        }]
    );
    for &(name, commits) in held_ones {
        assert_eq!(
            branch(e, name).verdict,
            held(push(commits), BranchHold::BusyUnknown),
            "{name}"
        );
    }
    for &(name, commits) in acting {
        assert_eq!(
            branch(e, name).verdict,
            Verdict::Act {
                action: push(commits)
            },
            "{name}"
        );
    }
}

#[test]
fn a_session_in_a_hand_made_git_dir_holds_the_branch_it_is_on() {
    for relative in [false, true] {
        let mut ws = FixtureWorkspace::new();
        let app = app_with_other(&mut ws);
        // a `.git` dir with a `commondir`, as a worktree's git dir has, that
        // no worktree list names
        let hand = ws.outside("hand");
        let git_dir = hand.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let common = if relative {
            "../../ws/app/.git\n".to_owned()
        } else {
            format!("{}\n", app.join(".git").display())
        };
        std::fs::write(git_dir.join("commondir"), common).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        ws.git(&hand, &["reset", "-q"]);
        assert_git_dirs(&ws, &hand, &git_dir, &app.join(".git"));
        assert_eq!(ws.git(&hand, &["symbolic-ref", "HEAD"]), "refs/heads/other");
        ws.commit(&hand, "hand");
        ws.assert_track(&app, "other", "[ahead 2]");
        assert!(!ws.git(&app, &["worktree", "list"]).contains("hand"));

        let on_other = Some(Head::Branch {
            name: "other".into(),
        });
        assert_unlisted(
            &ws,
            &hand,
            &git_dir,
            on_other.clone(),
            &[("other", 2)],
            &[("main", 1)],
        );
        // deeper in its files too
        std::fs::create_dir(hand.join("src")).unwrap();
        assert_unlisted(
            &ws,
            &hand.join("src"),
            &git_dir,
            on_other,
            &[("other", 2)],
            &[("main", 1)],
        );
    }
}

#[test]
fn a_hand_made_git_dirs_head_decides_what_it_holds() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let hand = ws.outside("hand");
    let git_dir = hand.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        git_dir.join("commondir"),
        format!("{}\n", app.join(".git").display()),
    )
    .unwrap();
    // detached: no branch moves
    let commit = ws.git(&app, &["rev-parse", "main"]);
    std::fs::write(git_dir.join("HEAD"), format!("{commit}\n")).unwrap();
    assert_git_dirs(&ws, &hand, &git_dir, &app.join(".git"));
    ws.assert_head(&hand, None);
    assert_eq!(ws.git(&hand, &["rev-parse", "HEAD"]), commit);
    assert_unlisted(
        &ws,
        &hand,
        &git_dir,
        Some(Head::Detached { commit }),
        &[],
        &[("main", 1), ("other", 1)],
    );
    // unknown: any branch might
    std::fs::write(git_dir.join("HEAD"), "ref: refs/tags/v1\n").unwrap();
    assert_eq!(ws.git(&hand, &["symbolic-ref", "HEAD"]), "refs/tags/v1");
    assert!(!ws.has_ref(&hand, "HEAD"));
    assert_unlisted(
        &ws,
        &hand,
        &git_dir,
        None,
        &[("main", 1), ("other", 1)],
        &[],
    );
}

/// A hand-made git dir at `hand` whose `commondir` is `commondir` and whose
/// `HEAD` is `head`, its index read from the commit git finds through them;
/// asserts git resolves its common dir as `common`.
fn hand_made(
    ws: &FixtureWorkspace,
    hand: &Path,
    commondir: &[u8],
    head: &[u8],
    common: &Path,
) -> PathBuf {
    let git_dir = hand.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("commondir"), commondir).unwrap();
    std::fs::write(git_dir.join("HEAD"), head).unwrap();
    ws.git(hand, &["reset", "-q"]);
    assert_git_dirs(ws, hand, &git_dir, common);
    git_dir
}

/// Asserts git, run in `checkout`, resolves its git dir as `git_dir` and
/// its common dir as `common`.
fn assert_git_dirs(ws: &FixtureWorkspace, checkout: &Path, git_dir: &Path, common: &Path) {
    let resolved = |args: &[&str]| PathBuf::from(ws.git(checkout, args));
    assert_eq!(resolved(&["rev-parse", "--absolute-git-dir"]), git_dir);
    assert_eq!(
        resolved(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
        common
    );
}

#[test]
fn a_hand_made_git_dirs_commondir_and_head_are_read_as_git_reads_them() {
    // past git's gitfile limit, which holds for neither file: git reads each
    // whole, however large
    let past_any_limit = |mut bytes: Vec<u8>| {
        bytes.resize(bytes.len() + MAX_GITFILE_BYTES + 64 * 1024, b'x');
        bytes
    };
    let on_other = b"ref: refs/heads/other\n".to_vec();
    // each a C string: cut at the first NUL
    let layouts: [(&str, &[u8], Vec<u8>); 3] = [
        (
            "HEAD cut at a NUL",
            b"\n",
            b"ref: refs/heads/other\0junk\n".to_vec(),
        ),
        (
            "HEAD read to a NUL past any size",
            b"\n",
            past_any_limit(b"ref: refs/heads/other\0".to_vec()),
        ),
        (
            "commondir read to a NUL past any size",
            &past_any_limit(b"\0".to_vec()),
            on_other,
        ),
    ];
    for (layout, commondir_tail, head) in layouts {
        let mut ws = FixtureWorkspace::new();
        let app = app_with_other(&mut ws);
        let hand = ws.outside("hand");
        let mut common = app.join(".git").into_os_string().into_encoded_bytes();
        common.extend_from_slice(commondir_tail);
        let git_dir = hand_made(&ws, &hand, &common, &head, &app.join(".git"));
        // git itself reads both so, and commits to `other` through them
        assert_eq!(
            ws.git(&hand, &["symbolic-ref", "HEAD"]),
            "refs/heads/other",
            "{layout}"
        );
        ws.commit(&hand, "hand");
        ws.assert_track(&app, "other", "[ahead 2]");

        assert_unlisted(
            &ws,
            &hand,
            &git_dir,
            Some(Head::Branch {
                name: "other".into(),
            }),
            &[("other", 2)],
            &[("main", 1)],
        );
    }
}

#[test]
fn a_session_in_a_git_dir_with_only_its_branches_linked_holds_the_branch_it_is_on() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let linked = ws.outside("linked");
    let git_dir = linked.join(".git");
    // a real `refs` with `heads` linked into the original's, and no
    // `commondir`: git keeps its branches there all the same
    std::fs::create_dir_all(git_dir.join("refs")).unwrap();
    for shared in ["config", "objects", "refs/heads"] {
        std::os::unix::fs::symlink(app.join(".git").join(shared), git_dir.join(shared)).unwrap();
    }
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
    ws.git(&linked, &["reset", "-q"]);
    ws.commit(&linked, "linked");
    ws.assert_track(&app, "other", "[ahead 2]");
    assert!(!git_dir.join("refs").is_symlink());

    assert_unlisted(
        &ws,
        &linked,
        &git_dir,
        Some(Head::Branch {
            name: "other".into(),
        }),
        &[("other", 2)],
        &[("main", 1)],
    );
}

#[test]
fn a_session_in_a_git_new_workdir_holds_the_branch_it_is_on() {
    let mut ws = FixtureWorkspace::new();
    let app = app_with_other(&mut ws);
    let new = ws.outside("new-workdir");
    let script = Path::new("/usr/share/doc/git/contrib/workdir/git-new-workdir");
    if script.is_file() {
        let out = ws
            .command("sh", &ws.root())
            .arg(script)
            .arg(&app)
            .arg(&new)
            .arg("other")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    } else {
        // as the script makes one: the shared parts linked, HEAD copied
        let git_dir = new.join(".git");
        std::fs::create_dir_all(git_dir.join("logs")).unwrap();
        for shared in [
            "config",
            "refs",
            "logs/refs",
            "objects",
            "info",
            "hooks",
            "packed-refs",
        ] {
            std::os::unix::fs::symlink(app.join(".git").join(shared), git_dir.join(shared))
                .unwrap();
        }
        std::fs::copy(app.join(".git/HEAD"), git_dir.join("HEAD")).unwrap();
        ws.git(&new, &["checkout", "-q", "-f", "other"]);
    }
    assert!(new.join(".git").join("refs").is_symlink());
    assert!(!new.join(".git").join("commondir").exists());
    ws.commit(&new, "new-workdir");
    ws.assert_track(&app, "other", "[ahead 2]");

    assert_unlisted(
        &ws,
        &new,
        &new.join(".git"),
        Some(Head::Branch {
            name: "other".into(),
        }),
        &[("other", 2)],
        &[("main", 1)],
    );
}
