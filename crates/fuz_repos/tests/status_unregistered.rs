//! The unregistered scan: the workspace root's children holding a `.git`
//! that the registry doesn't claim — clones, stray worktrees, and a
//! registered repo's worktrees that are moved, orphaned, or copied — while a
//! registered repo's live worktree, a registry dir by any path, and anything
//! without a `.git` stay out.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::path::{Path, PathBuf};

use fuz_repos::report::{RepairBlock, UnregisteredClone, UnregisteredKind};
use fuz_repos::state::Presence;
use support::{
    FixtureWorkspace, assert_git_dir_unchanged, owned_origin, seal, snapshot_git_dir,
    third_party_origin,
};

fn stray(
    dir: &str,
    origin: Option<&str>,
    owned: bool,
    kind: UnregisteredKind,
) -> UnregisteredClone {
    UnregisteredClone {
        dir: dir.into(),
        origin: origin.map(str::to_owned),
        owned,
        kind,
    }
}

fn moved(entry: &str) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: None,
        exit_noise: None,
    }
}

/// Moved, repairable, but git will complain about `noise` and exit 1.
fn moved_noisy(entry: &str, noise: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: None,
        exit_noise: Some(noise.to_str().unwrap().into()),
    }
}

/// Moved, but a repair would also rewrite `path`, which the worktree git
/// dir `git_dir` names.
fn moved_rewrites(entry: &str, path: &Path, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::Rewrites {
            path: path.to_str().unwrap().into(),
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

/// Moved, but the worktree git dir `git_dir` names this very dir.
fn moved_claimed(entry: &str, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::ClaimedDir {
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

/// Moved, but a worktree git dir of the repo, `git_dir`, names its worktree
/// relatively, so no repair is certain.
fn moved_relative(entry: &str, git_dir: &Path) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by: Some(RepairBlock::RelativeGitdir {
            git_dir: git_dir.to_str().unwrap().into(),
        }),
        exit_noise: None,
    }
}

/// `app`, an owned registered repo, clean on `main`.
fn app(ws: &mut FixtureWorkspace) -> PathBuf {
    let app = ws.owned_repo("app", &[]);
    ws.assert_clean(&app);
    app
}

/// A repo at `<root>/<dir>` with one commit and no remote.
fn init_repo(ws: &FixtureWorkspace, dir: &str) -> PathBuf {
    let path = ws.dir(dir);
    ws.git(
        &ws.root(),
        &["-c", "init.defaultBranch=main", "init", "-q", dir],
    );
    ws.commit(&path, "local");
    path
}

/// A clone of a new remote `name` outside the workspace, its origin set to
/// `origin`.
fn clone_outside(ws: &FixtureWorkspace, name: &str, origin: &str) -> PathBuf {
    ws.remote(name, &[]);
    let dest = ws.outside(name);
    ws.git(
        ws.base(),
        &[
            "clone",
            "-q",
            &format!("file://{}", ws.bare(name).display()),
            dest.to_str().unwrap(),
        ],
    );
    ws.set_origin(&dest, name, origin);
    assert!(!dest.starts_with(ws.root()));
    assert_eq!(
        ws.git(&dest, &["rev-parse", "--show-toplevel"]),
        dest.to_str().unwrap()
    );
    dest
}

/// `cp -r from to`: a copy that keeps every file, `.git` included.
fn copy_dir(ws: &FixtureWorkspace, from: &Path, to: &Path) {
    let out = ws
        .command("cp", ws.base())
        .args([OsStr::new("-r"), from.as_os_str(), to.as_os_str()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// The file a linked worktree's git dir names it by, as written.
fn gitdir_file(admin: &Path) -> String {
    std::fs::read_to_string(admin.join("gitdir"))
        .unwrap()
        .trim_end()
        .to_owned()
}

#[test]
fn owned_third_party_and_originless_clones_are_reported() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    ws.remote("mine", &[]);
    let mine = ws.clone_as("mine", "mine", &owned_origin("mine"), &[]);
    ws.remote("lib", &[]);
    let lib = ws.clone_as("lib", "lib", &third_party_origin("lib"), &[]);
    // a second url: a fetch uses the first, and so does the scan
    ws.git(
        &lib,
        &["config", "--add", "remote.origin.url", &owned_origin("lib")],
    );
    assert_eq!(
        ws.git(&lib, &["config", "--get-all", "remote.origin.url"]),
        format!("{}\n{}", third_party_origin("lib"), owned_origin("lib"))
    );
    let scratch = init_repo(&ws, "scratch");
    ws.git_fails(&scratch, &["config", "remote.origin.url"]);
    let before: Vec<_> = [&mine, &lib, &scratch]
        .iter()
        .map(|r| snapshot_git_dir(&r.join(".git")))
        .collect();

    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "lib",
                Some(&third_party_origin("lib")),
                false,
                UnregisteredKind::Clone
            ),
            stray(
                "mine",
                Some(&owned_origin("mine")),
                true,
                UnregisteredKind::Clone
            ),
            stray("scratch", None, false, UnregisteredKind::Clone),
        ]
    );
    // the scan, its git calls included, wrote nothing
    for (repo, before) in [&mine, &lib, &scratch].iter().zip(&before) {
        assert_git_dir_unchanged(before, &snapshot_git_dir(&repo.join(".git")));
    }
}

#[test]
fn things_without_a_git_are_ignored() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    support::write(&ws.dir("notes"), "todo.md", "x\n");
    support::write(&ws.root(), "loose.txt", "x\n");
    symlink(ws.outside("nowhere"), ws.dir("dangling")).unwrap();
    ws.git(&ws.root(), &["init", "-q", "--bare", "bare.git"]);
    // a registered entry's dir that isn't a repo is the probe's to report
    ws.declare_repo("empty", "empty", "");
    std::fs::create_dir(ws.dir("empty")).unwrap();
    assert!(ws.dir("bare.git").join("HEAD").is_file());

    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_live_worktree_of_a_registered_repo_is_skipped() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    assert_eq!(gitdir_file(&admin), feat.join(".git").to_str().unwrap());
    // and a detached one, reached through a symlink at the root
    let detached = ws.outside("app-detached");
    ws.add_worktree(&app, &detached, &["--detach"]);
    symlink(&detached, ws.dir("detached-link")).unwrap();

    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_is_reported_with_its_entry() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&feat, &moved_to).unwrap();
    // git still names the old path, and lists it as prunable
    assert_eq!(gitdir_file(&admin), feat.join(".git").to_str().unwrap());
    assert!(
        ws.worktree_record(&app, &feat)
            .iter()
            .any(|l| l == "prunable gitdir file points to non-existent location"),
        "{:?}",
        ws.worktree_record(&app, &feat)
    );
    // a worktree whose git dir names nothing: git doesn't list it
    let named = ws.dir("app-unnamed");
    let unnamed_admin = ws.add_worktree(&app, &named, &["-b", "unnamed"]);
    std::fs::remove_file(unnamed_admin.join("gitdir")).unwrap();
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("app-unnamed"), "{list}");
    let common = app.join(".git");
    let before = snapshot_git_dir(&common);

    assert_eq!(
        ws.unregistered(),
        [
            stray("app-moved", Some(&owned_origin("app")), true, moved("app")),
            stray(
                "app-unnamed",
                Some(&owned_origin("app")),
                true,
                moved("app")
            ),
        ]
    );
    assert_git_dir_unchanged(&before, &snapshot_git_dir(&common));

    // the advice holds: repair reconnects the moved one, and it's skipped
    ws.git(&app, &["worktree", "repair", moved_to.to_str().unwrap()]);
    ws.git(&app, &["worktree", "repair", named.to_str().unwrap()]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn an_orphaned_worktree_is_reported_with_its_entry() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let orphan = ws.dir("app-orphan");
    let admin = ws.add_worktree(&app, &orphan, &["-b", "orphan"]);
    std::fs::remove_dir_all(&admin).unwrap();
    // git can't use it, and repair can't reconnect it
    ws.git_fails(&orphan, &["status"]);
    ws.git_fails(&app, &["worktree", "repair", orphan.to_str().unwrap()]);

    // the origin comes from the repo whose git dir it named
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-orphan",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::OrphanedWorktree {
                entry: "app".into()
            },
        )]
    );
}

#[test]
fn a_copy_of_a_live_worktree_is_not_a_moved_one() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.outside("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &feat, &copy);
    // both name one git dir, which names the original
    assert_eq!(
        std::fs::read_to_string(copy.join(".git")).unwrap(),
        std::fs::read_to_string(feat.join(".git")).unwrap()
    );
    assert_eq!(gitdir_file(&admin), feat.join(".git").to_str().unwrap());

    // a repair here would take the git dir from the original
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: Some(feat.to_str().unwrap().into()),
            },
        )]
    );
}

#[test]
fn worktrees_of_an_unregistered_repo_are_strays() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    // an unregistered clone at the root, with a worktree beside it
    ws.remote("other", &[]);
    let other = ws.clone_as("other", "other", &third_party_origin("other"), &[]);
    ws.add_worktree(&other, &ws.dir("other-feat"), &["-b", "feat"]);
    // a repo outside the workspace, with a worktree inside it; its origin is
    // in the repo's config, read through the worktree
    let far = clone_outside(&ws, "far", &owned_origin("far"));
    let far_feat = ws.dir("far-feat");
    let admin = ws.add_worktree(&far, &far_feat, &["-b", "feat"]);
    assert!(!std::fs::read_to_string(admin.join("config")).is_ok_and(|c| c.contains("far")));
    // and an orphan of it
    let far_orphan = ws.dir("far-orphan");
    let orphan_admin = ws.add_worktree(&far, &far_orphan, &["-b", "orphan"]);
    std::fs::remove_dir_all(&orphan_admin).unwrap();

    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "far-feat",
                Some(&owned_origin("far")),
                true,
                UnregisteredKind::Worktree
            ),
            stray(
                "far-orphan",
                Some(&owned_origin("far")),
                true,
                UnregisteredKind::Worktree
            ),
            stray(
                "other",
                Some(&third_party_origin("other")),
                false,
                UnregisteredKind::Clone
            ),
            stray(
                "other-feat",
                Some(&third_party_origin("other")),
                false,
                UnregisteredKind::Worktree
            ),
        ]
    );
}

#[test]
fn a_symlink_is_judged_by_where_it_points() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    // to a registered dir: not a stray
    symlink(ws.dir("app"), ws.dir("app-link")).unwrap();
    // to a repo outside the workspace: a stray, by the link's name
    let far = clone_outside(&ws, "far", &third_party_origin("far"));
    symlink(&far, ws.dir("far-link")).unwrap();
    assert_eq!(ws.dir("far-link").canonicalize().unwrap(), far);

    assert_eq!(
        ws.unregistered(),
        [stray(
            "far-link",
            Some(&third_party_origin("far")),
            false,
            UnregisteredKind::Clone
        )]
    );
}

#[test]
fn discovery_never_reaches_a_root_that_is_a_repo() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    // the workspace root is itself a repo, with an origin
    let root = ws.root();
    ws.git(&root, &["-c", "init.defaultBranch=main", "init", "-q"]);
    ws.git(
        &root,
        &["remote", "add", "origin", &owned_origin("workspace")],
    );
    // a child whose `.git` git can't use: plain git walks up to the root's
    let broken = ws.dir("broken");
    std::fs::create_dir_all(broken.join(".git")).unwrap();
    assert_eq!(
        ws.git(&broken, &["config", "remote.origin.url"]),
        owned_origin("workspace")
    );

    assert_eq!(
        ws.unregistered(),
        [stray("broken", None, false, UnregisteredKind::Clone)]
    );
}

#[test]
fn an_included_config_gives_the_origin() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let stray_repo = init_repo(&ws, "included");
    let include = ws.outside("origin.inc");
    std::fs::write(
        &include,
        format!(
            "[remote \"origin\"]\n\turl = {}\n",
            owned_origin("included")
        ),
    )
    .unwrap();
    ws.git(
        &stray_repo,
        &["config", "include.path", include.to_str().unwrap()],
    );
    // not in the repo's own config file: only git's reading finds it
    let own = std::fs::read_to_string(stray_repo.join(".git/config")).unwrap();
    assert!(!own.contains("remote"), "{own}");
    assert_eq!(
        ws.git(&stray_repo, &["config", "remote.origin.url"]),
        owned_origin("included")
    );

    assert_eq!(
        ws.unregistered(),
        [stray(
            "included",
            Some(&owned_origin("included")),
            true,
            UnregisteredKind::Clone
        )]
    );
}

#[test]
fn a_name_that_is_not_utf8_is_reported_lossily() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let name = OsStr::from_bytes(b"odd\xff");
    let path = ws.root().join(name);
    std::fs::create_dir(&path).unwrap();
    let out = ws
        .command("git", &path)
        .args(["init", "-q"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(path.join(".git").is_dir());

    assert_eq!(
        ws.unregistered(),
        [stray("odd\u{fffd}", None, false, UnregisteredKind::Clone)]
    );
}

#[test]
fn a_copy_of_a_checkout_with_a_separate_git_dir_shares_it() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let git_dir = ws.outside("app.git");
    let app = ws.clone_owned(
        "app",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    assert!(app.join(".git").is_file());
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &app, &copy);
    assert_eq!(
        ws.git(&copy, &["rev-parse", "--absolute-git-dir"]),
        git_dir.to_str().unwrap()
    );
    // a `.git` linking to the separate git dir: shared with the checkout that
    // uses it, not the git dir's parent
    let link = ws.dir("app-link");
    std::fs::create_dir(&link).unwrap();
    symlink(&git_dir, link.join(".git")).unwrap();

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-copy", Some(&origin), true, shared("app", &app)),
            stray("app-link", Some(&origin), true, shared("app", &app)),
        ]
    );
}

fn shared(entry: &str, with: &Path) -> UnregisteredKind {
    UnregisteredKind::SharedGitDir {
        entry: entry.into(),
        with: Some(with.to_str().unwrap().into()),
    }
}

#[test]
fn copies_of_a_moved_worktree_share_its_git_dir_and_none_is_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // the git dir names a path that's gone
    let feat = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let (c1, c2) = (ws.dir("app-c1"), ws.dir("app-c2"));
    copy_dir(&ws, &feat, &c1);
    copy_dir(&ws, &feat, &c2);
    std::fs::remove_dir_all(&feat).unwrap();
    assert_eq!(gitdir_file(&admin), feat.join(".git").to_str().unwrap());
    // the git dir names nothing
    let g = ws.dir("app-g");
    let g_admin = ws.add_worktree(&app, &g, &["-b", "g"]);
    let (g1, g2) = (ws.dir("app-g1"), ws.dir("app-g2"));
    copy_dir(&ws, &g, &g1);
    copy_dir(&ws, &g, &g2);
    std::fs::remove_dir_all(&g).unwrap();
    std::fs::remove_file(g_admin.join("gitdir")).unwrap();
    for (copy, admin) in [
        (&c1, &admin),
        (&c2, &admin),
        (&g1, &g_admin),
        (&g2, &g_admin),
    ] {
        assert_eq!(
            PathBuf::from(ws.git(copy, &["rev-parse", "--absolute-git-dir"])),
            *admin
        );
    }

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-c1", Some(&origin), true, shared("app", &c2)),
            stray("app-c2", Some(&origin), true, shared("app", &c1)),
            stray("app-g1", Some(&origin), true, shared("app", &g2)),
            stray("app-g2", Some(&origin), true, shared("app", &g1)),
        ]
    );
}

#[test]
fn a_copy_of_a_locked_worktree_whose_original_is_absent_gets_no_fix() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let usb = ws.outside("usb");
    std::fs::create_dir(&usb).unwrap();
    let feat = usb.join("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    ws.git(
        &app,
        &[
            "worktree",
            "lock",
            "--reason",
            "on usb",
            feat.to_str().unwrap(),
        ],
    );
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &feat, &copy);
    // unmounted: the original is absent, and git keeps it, locked
    std::fs::rename(&usb, ws.outside("usb-unmounted")).unwrap();
    assert!(admin.join("locked").is_file());
    let record = ws.worktree_record(&app, &feat);
    assert!(record.iter().any(|l| l == "locked on usb"), "{record:?}");
    assert!(
        !record.iter().any(|l| l.starts_with("prunable")),
        "{record:?}"
    );

    // moved, or a copy of the absent original: a repair could take its git
    // dir, so it's refused
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            shared("app", &feat)
        )]
    );
}

#[test]
fn the_main_checkout_of_a_linked_registry_dir_is_not_a_stray() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    // the registry's dir is a linked worktree of a main checkout at the root
    let main = ws.clone_owned("app-main", "app", &[]);
    let app = ws.dir("app");
    ws.add_worktree(&main, &app, &["-b", "feat"]);
    let e = ws.entry("app");
    let checkouts: Vec<(&str, bool)> = e
        .checkouts
        .iter()
        .map(|c| (c.path.as_str(), c.linked))
        .collect();
    assert_eq!(
        checkouts,
        [
            (app.to_str().unwrap(), true),
            (main.to_str().unwrap(), false)
        ]
    );
    // a `.git` linking into its git dir, and a `.git` file naming it
    let link = ws.dir("link");
    std::fs::create_dir(&link).unwrap();
    symlink(main.join(".git"), link.join(".git")).unwrap();
    let named = ws.dir("named");
    support::write(
        &named,
        ".git",
        &format!("gitdir: {}\n", main.join(".git").display()),
    );

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("link", Some(&origin), true, shared("app", &main)),
            stray("named", Some(&origin), true, shared("app", &main)),
        ]
    );
}

#[test]
fn a_git_that_cannot_be_looked_at_is_still_reported() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let dangling = ws.dir("dangling");
    std::fs::create_dir(&dangling).unwrap();
    symlink(ws.outside("nowhere"), dangling.join(".git")).unwrap();
    assert!(!dangling.join(".git").exists());

    assert_eq!(
        ws.unregistered(),
        [stray("dangling", None, false, UnregisteredKind::Worktree)]
    );

    // unreadable: a repo's dir, and a `.git` file
    let sealed = init_repo(&ws, "sealed");
    let unreadable = ws.dir("unreadable");
    support::write(
        &unreadable,
        ".git",
        &format!("gitdir: {}\n", ws.dir("app").join(".git").display()),
    );
    let Some(_sealed) = seal(&sealed, 0o000) else {
        return;
    };
    let Some(_unreadable) = seal(&unreadable.join(".git"), 0o000) else {
        return;
    };
    assert_eq!(
        ws.unregistered(),
        [
            stray("dangling", None, false, UnregisteredKind::Worktree),
            stray("sealed", None, false, UnregisteredKind::Worktree),
            stray("unreadable", None, false, UnregisteredKind::Worktree),
        ]
    );
}

#[test]
fn a_worktree_git_dir_with_no_head_is_orphaned_not_moved() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // neither `gitdir` nor `HEAD`: the probe advises deleting it by hand
    // only when it keeps nothing
    let bare = ws.dir("app-bare");
    let admin = ws.add_worktree(&app, &bare, &["-b", "bare"]);
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    std::fs::remove_file(admin.join("HEAD")).unwrap();
    // `gitdir` naming this path, no `HEAD`
    let headless = ws.dir("app-headless");
    let headless_admin = ws.add_worktree(&app, &headless, &["-b", "headless"]);
    std::fs::remove_file(headless_admin.join("HEAD")).unwrap();
    std::fs::rename(&headless, ws.dir("app-headless-moved")).unwrap();
    for dir in ["app-bare", "app-headless-moved"] {
        ws.git_fails(&ws.dir(dir), &["status"]);
    }

    let origin = owned_origin("app");
    let orphaned = || UnregisteredKind::OrphanedWorktree {
        entry: "app".into(),
    };
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-bare", Some(&origin), true, orphaned()),
            stray("app-headless-moved", Some(&origin), true, orphaned()),
        ]
    );
}

#[test]
fn a_git_dir_outside_worktrees_is_not_a_live_worktree() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("wt");
    let admin = ws.add_worktree(&app, &wt, &["-b", "feat"]);
    // its git dir moved out of `worktrees/`, still naming this path, its
    // `commondir` pointing back: git works in it but doesn't list it
    let hidden = ws.outside("hidden");
    std::fs::rename(&admin, &hidden).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", hidden.display())).unwrap();
    std::fs::write(
        hidden.join("commondir"),
        format!("{}\n", app.join(".git").display()),
    )
    .unwrap();
    assert_eq!(gitdir_file(&hidden), wt.join(".git").to_str().unwrap());
    ws.assert_head(&wt, Some("feat"));
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains("/wt"), "{list}");

    assert_eq!(
        ws.unregistered(),
        [stray(
            "wt",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_copy_of_a_worktree_whose_git_cannot_be_read_gets_no_fix() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.outside("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &feat, &copy);
    assert_eq!(
        PathBuf::from(ws.git(&copy, &["rev-parse", "--absolute-git-dir"])),
        admin
    );
    // whether the original still uses the git dir is unknowable
    let Some(_sealed) = seal(&feat.join(".git"), 0o000) else {
        return;
    };

    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            shared("app", &feat)
        )]
    );
}

#[test]
fn a_copied_submodule_is_not_a_worktree_of_its_superproject() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // a submodule's `.git` names `<super git dir>/modules/<name>`: copied
    // out after that's gone, it names nothing
    let sub = ws.dir("sub-copy");
    let modules = app.join(".git/modules/sub");
    support::write(&sub, ".git", &format!("gitdir: {}\n", modules.display()));
    assert!(!modules.exists());

    assert_eq!(
        ws.unregistered(),
        [stray("sub-copy", None, false, UnregisteredKind::Worktree)]
    );
}

#[test]
fn ownership_ignores_case_and_an_empty_origin_is_none() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let upper = init_repo(&ws, "upper");
    ws.git(
        &upper,
        &["remote", "add", "origin", "git@github.com:ME/upper"],
    );
    let blank = init_repo(&ws, "blank");
    ws.git(&blank, &["config", "remote.origin.url", ""]);
    assert_eq!(
        ws.git_raw(&blank, &["config", "--get-all", "remote.origin.url"]),
        "\n"
    );

    assert_eq!(
        ws.unregistered(),
        [
            stray("blank", None, false, UnregisteredKind::Clone),
            stray(
                "upper",
                Some("git@github.com:ME/upper"),
                true,
                UnregisteredKind::Clone
            ),
        ]
    );
}

/// A worktree of `app` on a mount point outside the workspace — `<outside>/
/// usb/app-feat` — with a copy of it at `<root>/app-copy`; returns the
/// worktree and its git dir.
fn worktree_on_usb(ws: &FixtureWorkspace, app: &Path) -> (PathBuf, PathBuf) {
    let usb = ws.outside("usb");
    std::fs::create_dir(&usb).unwrap();
    let feat = usb.join("app-feat");
    let admin = ws.add_worktree(app, &feat, &["-b", "feat"]);
    copy_dir(ws, &feat, &ws.dir("app-copy"));
    (feat, admin)
}

fn shared_unnamed(entry: &str) -> UnregisteredKind {
    UnregisteredKind::SharedGitDir {
        entry: entry.into(),
        with: None,
    }
}

#[test]
fn a_locked_worktree_is_never_moved_whatever_its_gitdir_holds() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (feat, admin) = worktree_on_usb(&ws, &app);
    ws.git(&app, &["worktree", "lock", feat.to_str().unwrap()]);
    // its `gitdir` lost, and its media unmounted
    std::fs::remove_file(admin.join("gitdir")).unwrap();
    std::fs::rename(ws.outside("usb"), ws.outside("usb-unmounted")).unwrap();
    assert!(admin.join("locked").is_file() && admin.join("HEAD").is_file());
    // git keeps the git dir: a lock stops prune
    ws.git(&app, &["worktree", "prune"]);
    assert!(admin.is_dir());

    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            shared_unnamed("app")
        )]
    );

    // an empty `gitdir` is lost too
    std::fs::write(admin.join("gitdir"), "").unwrap();
    assert_eq!(ws.unregistered()[0].kind, shared_unnamed("app"));
    // unlocked (git can't, it no longer lists the worktree), a lost
    // `gitdir` is repair's to rewrite
    ws.git_fails(&app, &["worktree", "unlock", feat.to_str().unwrap()]);
    std::fs::remove_file(admin.join("locked")).unwrap();
    assert_eq!(ws.unregistered()[0].kind, moved("app"));
}

#[test]
fn a_gitdir_that_cannot_be_read_may_name_a_worktree_in_use() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (feat, admin) = worktree_on_usb(&ws, &app);
    // the original is live: git works in it
    let Some(_sealed) = seal(&admin.join("gitdir"), 0o000) else {
        return;
    };
    ws.assert_head(&feat, Some("feat"));

    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            shared_unnamed("app")
        )]
    );
}

#[test]
fn a_copy_whose_original_cannot_be_looked_at_gets_no_fix() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (feat, _) = worktree_on_usb(&ws, &app);
    // whether the original still uses the git dir is unknowable
    let Some(_sealed) = seal(&ws.outside("usb"), 0o000) else {
        return;
    };
    assert!(feat.join(".git").try_exists().is_err());

    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-copy",
            Some(&owned_origin("app")),
            true,
            shared("app", &feat)
        )]
    );
}

#[test]
fn a_full_checkout_a_registry_dir_links_to_is_shared_not_skipped() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    let real = ws.clone_owned("app-real", "app", &[]);
    // the registry's dir uses `app-real`'s git dir, through a `.git` link
    let app = ws.dir("app");
    std::fs::create_dir(&app).unwrap();
    symlink(real.join(".git"), app.join(".git")).unwrap();
    ws.assert_head(&app, Some("main"));
    assert_eq!(ws.entry("app").presence, Presence::Present);
    let origin = owned_origin("app");
    let want = [stray("app-real", Some(&origin), true, shared("app", &app))];
    assert_eq!(ws.unregistered(), want);

    // or through a `.git` file
    std::fs::remove_file(app.join(".git")).unwrap();
    support::write(
        &app,
        ".git",
        &format!("gitdir: {}\n", real.join(".git").display()),
    );
    ws.assert_head(&app, Some("main"));
    assert_eq!(ws.unregistered(), want);
}

#[test]
fn a_symlink_to_a_stray_is_the_same_checkout() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.dir("app-feat");
    ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&feat, &moved_to).unwrap();
    symlink(&moved_to, ws.dir("zz-alias")).unwrap();
    // and one to a clone: each name is reported
    init_repo(&ws, "mine");
    symlink(ws.dir("mine"), ws.dir("mine-alias")).unwrap();

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-moved", Some(&origin), true, moved("app")),
            stray("mine", None, false, UnregisteredKind::Clone),
            stray("mine-alias", None, false, UnregisteredKind::Clone),
            stray("zz-alias", Some(&origin), true, moved("app")),
        ]
    );
}

#[test]
fn a_git_linked_into_a_worktree_git_dir_shares_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // a live worktree, and a `.git` linking into its git dir
    let feat = ws.outside("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let link = ws.dir("link");
    std::fs::create_dir(&link).unwrap();
    symlink(&admin, link.join(".git")).unwrap();
    // a moved worktree, and a `.git` linking into its git dir: a repair of
    // either would take it from the other
    let gone = ws.dir("app-gone");
    let gone_admin = ws.add_worktree(&app, &gone, &["-b", "gone"]);
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&gone, &moved_to).unwrap();
    let gone_link = ws.dir("gone-link");
    std::fs::create_dir(&gone_link).unwrap();
    symlink(&gone_admin, gone_link.join(".git")).unwrap();
    ws.assert_head(&link, Some("feat"));
    ws.assert_head(&gone_link, Some("gone"));

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-moved", Some(&origin), true, shared("app", &gone_link)),
            stray("gone-link", Some(&origin), true, shared("app", &moved_to)),
            stray("link", Some(&origin), true, shared("app", &feat)),
        ]
    );
}

#[test]
fn a_relative_gitdir_resolves_against_the_git_dir() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // as git 2.48's `worktree.useRelativePaths` writes them
    let feat = ws.dir("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    std::fs::write(admin.join("gitdir"), "../../../../app-feat/.git\n").unwrap();
    std::fs::write(
        feat.join(".git"),
        "gitdir: ../app/.git/worktrees/app-feat\n",
    )
    .unwrap();
    assert_eq!(
        admin
            .join("../../../../app-feat/.git")
            .canonicalize()
            .unwrap(),
        feat.join(".git")
    );
    ws.assert_head(&feat, Some("feat"));
    assert_eq!(ws.unregistered(), []);

    // moved: the relative path names where it was, but git versions
    // resolve it differently, so no repair is offered
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&feat, &moved_to).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-moved",
            Some(&owned_origin("app")),
            true,
            moved_relative("app", &admin)
        )]
    );
}

#[test]
fn a_worktree_git_dir_whose_commondir_cannot_be_read() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.outside("app-feat");
    let admin = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    copy_dir(&ws, &feat, &ws.dir("app-copy"));
    let Some(_sealed) = seal(&admin.join("commondir"), 0o000) else {
        return;
    };

    assert_eq!(
        ws.unregistered(),
        [stray("app-copy", None, false, UnregisteredKind::Worktree)]
    );
}

/// Where a checkout's `.git` file points, by the git dir's name.
fn points_at(checkout: &Path) -> String {
    let target = std::fs::read_to_string(checkout.join(".git")).unwrap();
    Path::new(target.trim_end())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn swapped_worktrees_are_repaired_one_safe_step_at_a_time() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.dir("app-feat");
    ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let new = ws.dir("app-new");
    ws.add_worktree(&app, &new, &["-b", "new"]);
    support::write(&new, "n.txt", "new work\n");
    ws.git(&new, &["add", "n.txt"]);
    // renamed by hand: app-feat to app-old, then app-new to app-feat
    let old = ws.dir("app-old");
    std::fs::rename(&feat, &old).unwrap();
    std::fs::rename(&new, &feat).unwrap();
    assert_eq!(points_at(&old), "app-feat");
    assert_eq!(points_at(&feat), "app-new");
    let admin = |id: &str| app.join(".git/worktrees").join(id);
    assert_eq!(
        gitdir_file(&admin("app-feat")),
        feat.join(".git").to_str().unwrap()
    );

    // git dir `app-feat` names the path `app-feat` now holds, whose `.git`
    // names `app-new`: repairing that one first would take `app-feat`'s
    // git dir for it; app-old's repair is safe
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "app-feat",
                Some(&origin),
                true,
                moved_claimed("app", &admin("app-feat"))
            ),
            stray("app-old", Some(&origin), true, moved("app")),
        ]
    );

    ws.git(&app, &["worktree", "repair", old.to_str().unwrap()]);
    assert_eq!(points_at(&old), "app-feat");
    assert_eq!(points_at(&feat), "app-new");
    ws.assert_head(&old, Some("feat"));
    ws.assert_head(&feat, Some("new"));
    ws.assert_porcelain(&feat, &["A  n.txt"]);
    // the next run offers the other
    assert_eq!(
        ws.unregistered(),
        [stray("app-feat", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", feat.to_str().unwrap()]);
    ws.assert_head(&feat, Some("new"));
    ws.assert_porcelain(&feat, &["A  n.txt"]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_repair_that_would_rewrite_another_checkout_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `q` names `<root>/q`, which now holds the worktree of git dir
    // `q2` (swapped by hand, the original deleted)
    let q = ws.dir("q");
    let q_admin = ws.add_worktree(&app, &q, &["-b", "y"]);
    let q2 = ws.dir("q2");
    ws.add_worktree(&app, &q2, &["-b", "y2"]);
    std::fs::remove_dir_all(&q).unwrap();
    std::fs::rename(&q2, &q).unwrap();
    assert_eq!(points_at(&q), "q2");
    // an unrelated worktree, moved
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("q", Some(&origin), true, moved_claimed("app", &q_admin)),
            stray(
                "s-moved",
                Some(&origin),
                true,
                moved_rewrites("app", &q, &q_admin)
            ),
        ]
    );

    // what the refused repair would do: take git dir `q` for `q`'s checkout
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(points_at(&q), "q");
}

#[test]
fn a_repair_that_would_write_into_a_plain_dir_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a dir that's now plain files, no `.git`
    let else_dir = ws.outside("else");
    std::fs::create_dir(&else_dir).unwrap();
    let y = else_dir.join("y");
    let y_admin = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    support::write(&y, "notes.txt", "mine\n");
    // and one git would only complain about: a blocked repair carries no
    // noise
    let z = else_dir.join("z");
    ws.add_worktree(&app, &z, &["-b", "z"]);
    std::fs::remove_dir_all(&z).unwrap();
    std::fs::write(&z, "a file\n").unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &y, &y_admin)
        )]
    );

    // what the refused repair would do: write a `.git` into it (exiting 1,
    // over `z`)
    ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(y.join(".git").is_file());
}

#[test]
fn a_git_link_into_a_moved_worktrees_git_dir_is_not_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let far = ws.outside("far");
    std::fs::create_dir(&far).unwrap();
    let admin = ws.add_worktree(&app, &far.join("wf"), &["-b", "wf"]);
    let link = ws.dir("link");
    std::fs::create_dir(&link).unwrap();
    symlink(&admin, link.join(".git")).unwrap();
    std::fs::remove_dir_all(&far).unwrap();
    ws.assert_head(&link, Some("wf"));
    // git won't repair through a `.git` that isn't a file
    ws.git_fails(&app, &["worktree", "repair", link.to_str().unwrap()]);

    assert_eq!(
        ws.unregistered(),
        [stray(
            "link",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn copies_of_a_main_checkout_git_cannot_find_share_its_git_dir() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    // the main checkout keeps its git dir outside the workspace, and the
    // registry's dir is a linked worktree of it
    let git_dir = ws.outside("app.git");
    let main = ws.clone_owned(
        "app-main",
        "app",
        &["--separate-git-dir", git_dir.to_str().unwrap()],
    );
    let app = ws.dir("app");
    ws.add_worktree(&main, &app, &["-b", "feat"]);
    assert_eq!(
        ws.git(
            &app,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        git_dir.to_str().unwrap()
    );
    // alone, nothing says which checkout the git dir is the main one of
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray(
            "app-main",
            Some(&origin),
            true,
            UnregisteredKind::Clone
        )]
    );

    let copy = ws.dir("app-main-copy");
    copy_dir(&ws, &main, &copy);
    assert_eq!(
        ws.unregistered(),
        [
            stray("app-main", Some(&origin), true, shared("app", &copy)),
            stray("app-main-copy", Some(&origin), true, shared("app", &main)),
        ]
    );
}

#[test]
fn a_named_path_holding_a_git_dir_is_left_alone_by_a_repair() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a dir that now holds a repo of its own: git skips a
    // `.git` that isn't a file
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    ws.git(
        ws.base(),
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "-q",
            y.to_str().unwrap(),
        ],
    );
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // the repair reconnects it, complaining about `y` (exit 1) and leaving
    // it be
    let before = snapshot_git_dir(&y.join(".git"));
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".git is not a file"), "{stderr}");
    assert_git_dir_unchanged(&before, &snapshot_git_dir(&y.join(".git")));
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_git_is_a_link_is_not_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // its `.git` a link to a gitfile kept elsewhere
    let x = ws.dir("x");
    ws.add_worktree(&app, &x, &["-b", "x"]);
    let store = ws.outside("store");
    std::fs::create_dir(&store).unwrap();
    std::fs::rename(x.join(".git"), store.join("x.gitfile")).unwrap();
    symlink(store.join("x.gitfile"), x.join(".git")).unwrap();
    ws.assert_head(&x, Some("x"));
    // live, it's skipped
    assert_eq!(ws.unregistered(), []);

    // moved: a repair would write the link's target into the git dir
    std::fs::rename(&x, ws.dir("x-moved")).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "x-moved",
            Some(&owned_origin("app")),
            true,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_gitdir_without_a_git_suffix_names_the_dir_itself() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    // git dir `k` names `<app>/sub` by hand: git takes it as the worktree
    // itself, not `<app>`
    let k = ws.outside("k");
    let k_admin = ws.add_worktree(&app, &k, &["-b", "k"]);
    let sub = app.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(k_admin.join("gitdir"), format!("{}\n", sub.display())).unwrap();
    assert_eq!(
        ws.worktree_record(&app, &sub)[0],
        format!("worktree {}", sub.display())
    );

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &sub, &k_admin)
        )]
    );
    // what the refused repair would do: write a `.git` into the main checkout
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(sub.join(".git").is_file());
}

#[test]
fn a_hazard_git_that_is_broken_or_a_link_is_judged_as_git_does() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    // `h` elsewhere, its `.git` a link to a gitfile naming its own git dir:
    // git follows the link, finds it right, and leaves it be
    let h = ws.outside("h");
    let h_admin = ws.add_worktree(&app, &h, &["-b", "h"]);
    let gitfile = ws.outside("h.gitfile");
    std::fs::rename(h.join(".git"), &gitfile).unwrap();
    symlink(&gitfile, h.join(".git")).unwrap();
    ws.assert_head(&h, Some("h"));
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );

    // the link's gitfile naming another git dir: git would rewrite it
    let other = app.join(".git/worktrees/s");
    std::fs::write(&gitfile, format!("gitdir: {}\n", other.display())).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_admin)
        )]
    );

    // a `.git` file git can't parse: broken, git would rewrite it
    std::fs::remove_file(h.join(".git")).unwrap();
    std::fs::write(h.join(".git"), "not a gitfile\n").unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_admin)
        )]
    );
}

#[test]
fn two_swapped_worktrees_are_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wa = ws.dir("wa");
    let wa_admin = ws.add_worktree(&app, &wa, &["-b", "a"]);
    let wb = ws.dir("wb");
    let wb_admin = ws.add_worktree(&app, &wb, &["-b", "b"]);
    let tmp = ws.dir("tmp");
    std::fs::rename(&wa, &tmp).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    std::fs::rename(&tmp, &wb).unwrap();
    assert_eq!(points_at(&wa), "wb");
    assert_eq!(points_at(&wb), "wa");

    let origin = owned_origin("app");
    let swapped = |git_dir: &Path, with: &str| UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Swapped {
            git_dir: git_dir.to_str().unwrap().into(),
            with: with.into(),
        }),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [
            stray("wa", Some(&origin), true, swapped(&wa_admin, "wb")),
            stray("wb", Some(&origin), true, swapped(&wb_admin, "wa")),
        ]
    );

    // the advice holds: moved back, both are live
    std::fs::rename(&wa, &tmp).unwrap();
    std::fs::rename(&wb, &wa).unwrap();
    std::fs::rename(&tmp, &wb).unwrap();
    assert_eq!(ws.unregistered(), []);

    // a three-way rotation isn't a swap: wa's claimant has its own dir
    // claimed, but by another git dir than wa's
    let (ws, admin) = three_worktrees(&[("wa", "tmp"), ("wc", "wa"), ("wb", "wc"), ("tmp", "wb")]);
    assert_eq!(points_at(&ws.dir("wa")), "wc");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "wa",
                Some(&origin),
                true,
                moved_claimed("app", &admin("wa"))
            ),
            stray(
                "wb",
                Some(&origin),
                true,
                moved_claimed("app", &admin("wb"))
            ),
            stray(
                "wc",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wa"), &admin("wa"))
            ),
        ]
    );

    // nor is a three-step chain: wa's claimant is blocked by another dir
    // than its own
    let (ws, admin) = three_worktrees(&[("wa", "wa-old"), ("wb", "wa"), ("wc", "wb")]);
    assert_eq!(points_at(&ws.dir("wa-old")), "wa");
    assert_eq!(
        ws.unregistered(),
        [
            stray(
                "wa",
                Some(&origin),
                true,
                moved_claimed("app", &admin("wa"))
            ),
            stray(
                "wa-old",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wb"), &admin("wb"))
            ),
            stray(
                "wb",
                Some(&origin),
                true,
                moved_rewrites("app", &ws.dir("wa"), &admin("wa"))
            ),
        ]
    );
}

/// `app` with worktrees wa, wb, wc at the root, then the renames `moves`
/// made by hand, in order; returns the workspace and a worktree git dir by
/// id.
fn three_worktrees(moves: &[(&str, &str)]) -> (FixtureWorkspace, impl Fn(&str) -> PathBuf) {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    for id in ["wa", "wb", "wc"] {
        ws.add_worktree(&app, &ws.dir(id), &["-b", id]);
    }
    for (from, to) in moves {
        std::fs::rename(ws.dir(from), ws.dir(to)).unwrap();
    }
    let worktrees = app.join(".git/worktrees");
    (ws, move |id: &str| worktrees.join(id))
}

#[test]
fn a_repair_git_complains_through_is_offered_with_its_noise() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a path that's now a plain file
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    std::fs::write(&y, "a file\n").unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // git complains and exits 1, repairing it all the same
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&y).unwrap(), "a file\n");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_dangling_link_at_a_worktrees_path_is_noise() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names a path that's now a dangling link: git looks at the
    // link itself, not its target, so it walks `y` and complains
    let y = ws.outside("y");
    ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_dir_all(&y).unwrap();
    let nowhere = ws.outside("nowhere");
    symlink(&nowhere, &y).unwrap();
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &y)
        )]
    );
    // git complains and exits 1, repairing it all the same
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
    assert_eq!(std::fs::read_link(&y).unwrap(), nowhere);
    assert!(!nowhere.exists());
    assert_eq!(ws.unregistered(), []);
}

/// Runs `f` on its own thread, failing (not hanging) the test when it
/// doesn't finish within a minute.
fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let done = rx.recv_timeout(std::time::Duration::from_secs(60));
    assert!(done.is_ok(), "blocked: not finished within a minute");
    done.unwrap()
}

/// Makes a FIFO at `path`.
fn mkfifo(ws: &FixtureWorkspace, path: &Path) {
    let out = ws.command("mkfifo", ws.base()).arg(path).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(std::fs::metadata(path).unwrap().file_type().is_fifo());
}

#[test]
fn a_git_that_is_a_fifo_is_reported_not_read() {
    let mut ws = FixtureWorkspace::new();
    app(&mut ws);
    let fifo = ws.dir("fifo");
    std::fs::create_dir(&fifo).unwrap();
    mkfifo(&ws, &fifo.join(".git"));
    // git fails fast on it
    ws.git_fails(&fifo, &["status"]);

    let found = within_a_minute(move || ws.unregistered());
    assert_eq!(
        found,
        [stray("fifo", None, false, UnregisteredKind::Worktree)]
    );
}

#[test]
fn a_repair_leaves_the_old_path_of_its_own_git_dir_alone() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // moved out; its old path now holds a repo of its own: git's walk would
    // complain about it if another git dir named it, but this one's is
    // repointed first
    let old = ws.outside("s");
    ws.add_worktree(&app, &old, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&old, &s_moved).unwrap();
    ws.git(
        ws.base(),
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "-q",
            old.to_str().unwrap(),
        ],
    );

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved("app")
        )]
    );
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_old_path_is_now_a_file_is_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let old = ws.outside("s");
    ws.add_worktree(&app, &old, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&old, &s_moved).unwrap();
    std::fs::write(&old, "a file\n").unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved("app")
        )]
    );
    let out = ws.git_output(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "a file\n");
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_blocking_path_shows_as_the_git_dir_writes_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names its worktree through a link: git's messages name
    // the linked path, and so does the report
    let real = ws.outside("real");
    std::fs::create_dir(&real).unwrap();
    let link = ws.outside("link");
    symlink(&real, &link).unwrap();
    let y = real.join("y");
    let y_admin = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_file(y.join(".git")).unwrap();
    let written = link.join("y");
    std::fs::write(
        y_admin.join("gitdir"),
        format!("{}\n", written.join(".git").display()),
    )
    .unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_rewrites("app", &written, &y_admin)
        )]
    );
}

#[test]
fn a_relative_gitdir_blocks_every_repair_in_its_repo() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `k` names its live worktree relatively: git 2.48+ resolves it
    // against the git dir, older gits against the cwd — where a repair
    // would write a `.git` into whatever dir that names
    let k = ws.outside("k");
    let k_admin = ws.add_worktree(&app, &k, &["-b", "k"]);
    std::fs::write(k_admin.join("gitdir"), "../../../../../k/.git\n").unwrap();
    assert_eq!(k_admin.join("../../../../../k").canonicalize().unwrap(), k);
    // an unrelated worktree, moved
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();
    let origin = owned_origin("app");

    let blocked = [stray(
        "s-moved",
        Some(&origin),
        true,
        moved_relative("app", &k_admin),
    )];
    assert_eq!(ws.unregistered(), blocked);
    // and whatever else stands in the way, the relative gitdir comes first:
    // `k`'s `.git` gone (a rewrite), then a dir (noise)
    std::fs::remove_file(k.join(".git")).unwrap();
    assert_eq!(ws.unregistered(), blocked);
    std::fs::create_dir(k.join(".git")).unwrap();
    assert_eq!(ws.unregistered(), blocked);
}

#[test]
fn a_noisy_path_shows_as_the_git_dir_writes_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git dir `y` names its worktree through a link, and its `.git` is a
    // dir: git complains about the linked path, and so does the report
    let real = ws.outside("real");
    std::fs::create_dir(&real).unwrap();
    let link = ws.outside("link");
    symlink(&real, &link).unwrap();
    let y = real.join("y");
    let y_admin = ws.add_worktree(&app, &y, &["-b", "y"]);
    std::fs::remove_file(y.join(".git")).unwrap();
    std::fs::create_dir(y.join(".git")).unwrap();
    let written = link.join("y");
    std::fs::write(
        y_admin.join("gitdir"),
        format!("{}\n", written.join(".git").display()),
    )
    .unwrap();
    let s_dir = ws.dir("s");
    ws.add_worktree(&app, &s_dir, &["-b", "s"]);
    let s_moved = ws.dir("s-moved");
    std::fs::rename(&s_dir, &s_moved).unwrap();

    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-moved",
            Some(&owned_origin("app")),
            true,
            moved_noisy("app", &written)
        )]
    );
}

/// A worktree of `app` added at `<root>/<name>`, then moved by hand to
/// `<root>/<name>-moved`; returns the new path and its git dir.
fn moved_by_hand(ws: &FixtureWorkspace, app: &Path, name: &str) -> (PathBuf, PathBuf) {
    let wt = ws.dir(name);
    let admin = ws.add_worktree(app, &wt, &["-b", name]);
    let moved_to = ws.dir(&format!("{name}-moved"));
    std::fs::rename(&wt, &moved_to).unwrap();
    (moved_to, admin)
}

/// A gitfile naming `git_dir`, with `head` before it and `tail` after.
fn gitfile(head: &[u8], git_dir: &Path, tail: &[u8]) -> Vec<u8> {
    [head, b"gitdir: ", git_dir.as_os_str().as_bytes(), tail].concat()
}

#[test]
fn a_strays_gitfile_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git follows a path cut at a NUL
    let (nul, nul_admin) = moved_by_hand(&ws, &app, "nul");
    std::fs::write(nul.join(".git"), gitfile(b"", &nul_admin, b"\0junk\n")).unwrap();
    ws.assert_head(&nul, Some("nul"));
    // and refuses `gitdir: ` on a second line
    let (second, second_admin) = moved_by_hand(&ws, &app, "second");
    std::fs::write(second.join(".git"), gitfile(b"x\n", &second_admin, b"\n")).unwrap();
    ws.git_fails(&second, &["status"]);

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("nul-moved", Some(&origin), true, moved("app")),
            stray("second-moved", None, false, UnregisteredKind::Worktree),
        ]
    );
    // the advice holds: repair reconnects the one git follows
    ws.git(&app, &["worktree", "repair", nul.to_str().unwrap()]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "second-moved",
            None,
            false,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_moved_worktrees_commondir_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // git follows a `commondir` cut at a NUL
    let (nul, nul_admin) = moved_by_hand(&ws, &app, "nul");
    std::fs::write(nul_admin.join("commondir"), b"../..\0junk\n").unwrap();
    ws.assert_head(&nul, Some("nul"));
    // and keeps a trailing space, naming no common dir
    let (spaced, spaced_admin) = moved_by_hand(&ws, &app, "spaced");
    std::fs::write(spaced_admin.join("commondir"), b"../.. \n").unwrap();
    ws.git_fails(&spaced, &["status"]);

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("nul-moved", Some(&origin), true, moved("app")),
            stray("spaced-moved", None, false, UnregisteredKind::Worktree),
        ]
    );
    // the advice holds: repaired, it's a live worktree of `app`
    ws.git(&app, &["worktree", "repair", nul.to_str().unwrap()]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "spaced-moved",
            None,
            false,
            UnregisteredKind::Worktree
        )]
    );
}

#[test]
fn a_hazard_gitfile_is_parsed_as_git_parses_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `h` elsewhere, its `.git` naming its own git dir past a NUL: git reads
    // it right, and a repair leaves it be
    let h = ws.outside("h");
    let h_admin = ws.add_worktree(&app, &h, &["-b", "h"]);
    let past_nul = gitfile(b"", &h_admin, b"\0junk\n");
    std::fs::write(h.join(".git"), &past_nul).unwrap();
    ws.assert_head(&h, Some("h"));
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(std::fs::read(h.join(".git")).unwrap(), past_nul);
    assert_eq!(ws.unregistered(), []);

    // on a second line: git can't parse it, so a repair would rewrite it
    let s_again = ws.dir("s-again");
    std::fs::rename(&s_moved, &s_again).unwrap();
    std::fs::write(h.join(".git"), gitfile(b"x\n", &h_admin, b"\n")).unwrap();
    ws.git_fails(&h, &["status"]);
    assert_eq!(
        ws.unregistered(),
        [stray(
            "s-again",
            Some(&origin),
            true,
            moved_rewrites("app", &h, &h_admin)
        )]
    );
    // as it does, when run anyway
    ws.git_output(&app, &["worktree", "repair", s_again.to_str().unwrap()]);
    let rewritten = std::fs::read_to_string(h.join(".git")).unwrap();
    assert!(rewritten.starts_with("gitdir: "), "{rewritten:?}");
    ws.assert_head(&h, Some("h"));
}

/// `git -C <app> worktree add -q <path> <args>` for a path that may not be
/// UTF-8; returns its git dir, `<common>/worktrees/<id>`.
fn add_worktree_at(ws: &FixtureWorkspace, app: &Path, path: &Path, args: &[&str]) -> PathBuf {
    let out = ws
        .command("git", app)
        .args([OsStr::new("worktree"), OsStr::new("add"), OsStr::new("-q")])
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let out = ws.git_output(path, &["rev-parse", "--absolute-git-dir"]);
    assert!(out.status.success(), "{out:?}");
    let admin = out.stdout.strip_suffix(b"\n").unwrap();
    PathBuf::from(OsStr::from_bytes(admin))
}

/// Swaps two dirs by hand.
fn swap_dirs(ws: &FixtureWorkspace, a: &Path, b: &Path) {
    let tmp = ws.dir("swap-tmp");
    std::fs::rename(a, &tmp).unwrap();
    std::fs::rename(b, a).unwrap();
    std::fs::rename(&tmp, b).unwrap();
}

/// `git worktree list --porcelain`'s raw bytes, paths as git lists them.
fn worktree_list(ws: &FixtureWorkspace, app: &Path) -> Vec<u8> {
    let out = ws
        .command("git", app)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    out.stdout
}

/// Whether git lists a worktree at exactly `path`, bytes and all.
fn lists_worktree(list: &[u8], path: &Path) -> bool {
    let line = [b"worktree ", path.as_os_str().as_bytes(), b"\n"].concat();
    list.windows(line.len()).any(|w| w == line)
}

/// Moved, but swapped by hand with `with`: `git_dir` names this dir.
fn swapped(git_dir: &Path, with: &str) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Swapped {
            git_dir: git_dir.to_string_lossy().into_owned(),
            with: with.into(),
        }),
        exit_noise: None,
    }
}

#[test]
fn a_swap_with_a_worktree_whose_path_is_not_utf8_is_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let a = ws.dir("a");
    let a_admin = add_worktree_at(&ws, &app, &a, &["-b", "a"]);
    let b = ws.root().join(OsStr::from_bytes(b"b\xff"));
    let b_admin = add_worktree_at(&ws, &app, &b, &["-b", "b"]);
    assert_eq!(b_admin.file_name(), Some(OsStr::from_bytes(b"b\xff")));
    swap_dirs(&ws, &a, &b);
    // git's view: each git dir still names its old path, each dir holds the
    // other's checkout
    let list = worktree_list(&ws, &app);
    assert!(lists_worktree(&list, &a) && lists_worktree(&list, &b));
    ws.assert_head(&a, Some("b"));
    ws.assert_head(&b, Some("a"));

    // no repair: either would hijack the other
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("a", Some(&origin), true, swapped(&a_admin, "b\u{fffd}")),
            stray("b\u{fffd}", Some(&origin), true, swapped(&b_admin, "a")),
        ]
    );
    // the advice holds: moved back, both are live
    swap_dirs(&ws, &a, &b);
    ws.assert_head(&a, Some("a"));
    ws.assert_head(&b, Some("b"));
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_swap_after_a_move_to_a_path_that_is_not_utf8_is_told_to_move_back() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let a = ws.dir("a");
    let a_admin = add_worktree_at(&ws, &app, &a, &["-b", "a"]);
    let b_admin = add_worktree_at(&ws, &app, &ws.dir("b"), &["-b", "b"]);
    // moved by git, so its git dir keeps its UTF-8 id and names the new path
    let b = ws.root().join(OsStr::from_bytes(b"b\xff"));
    let out = ws
        .command("git", &app)
        .args(["worktree", "move"])
        .arg(ws.dir("b"))
        .arg(&b)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(lists_worktree(&worktree_list(&ws, &app), &b));
    assert_eq!(ws.unregistered(), []);
    swap_dirs(&ws, &a, &b);
    ws.assert_head(&a, Some("b"));
    ws.assert_head(&b, Some("a"));

    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [
            stray("a", Some(&origin), true, swapped(&a_admin, "b\u{fffd}")),
            stray("b\u{fffd}", Some(&origin), true, swapped(&b_admin, "a")),
        ]
    );
    swap_dirs(&ws, &a, &b);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_nul_in_a_worktree_git_dirs_gitdir_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let w = ws.dir("w");
    let admin = ws.add_worktree(&app, &w, &["-b", "w"]);
    // git strips a trailing `/.git` from the whole file, then cuts at the
    // NUL: it lists the worktree at `w/.git`, and `w` isn't live
    let gitdir = [w.join(".git").as_os_str().as_bytes(), b"\0junk\n"].concat();
    std::fs::write(admin.join("gitdir"), &gitdir).unwrap();
    ws.worktree_record(&app, &w.join(".git"));
    // yet a repair of `w` compares what's before the NUL with `w/.git`, sees
    // nothing to fix, and fails on the walk
    ws.git_fails(&app, &["worktree", "repair", w.to_str().unwrap()]);
    assert_eq!(std::fs::read(admin.join("gitdir")).unwrap(), gitdir);

    let origin = owned_origin("app");
    let nul = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::NulInGitdir {
            git_dir: admin.to_str().unwrap().into(),
        }),
        exit_noise: None,
    };
    assert_eq!(ws.unregistered(), [stray("w", Some(&origin), true, nul)]);
    // the advice holds: `w/.git` written into its gitdir by hand, it's live
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", w.join(".git").display()),
    )
    .unwrap();
    ws.worktree_record(&app, &w);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_gitdir_past_the_tools_limit_blocks_every_repair_in_its_repo() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `h`'s gitdir padded past the tool's limit: git reads it whole and
    // lists `h`, but the tool can't tell what a repair's walk does with it
    let h = ws.dir("h");
    let h_admin = ws.add_worktree(&app, &h, &["-b", "h"]);
    let mut padded = h.join(".git").as_os_str().as_bytes().to_vec();
    padded.resize(2 * 1024 * 1024, b'\n');
    std::fs::write(h_admin.join("gitdir"), &padded).unwrap();
    ws.worktree_record(&app, &h);

    let origin = owned_origin("app");
    let unreadable = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::UnreadableGitdir {
            git_dir: h_admin.to_str().unwrap().into(),
        }),
        exit_noise: None,
    };
    // and `h` itself, which the tool can't tell is live, fails closed too
    assert_eq!(
        ws.unregistered(),
        [
            stray("h", Some(&origin), true, shared_unnamed("app")),
            stray("s-moved", Some(&origin), true, unreadable),
        ]
    );
    // trimmed back, the repair is offered, and holds
    std::fs::write(
        h_admin.join("gitdir"),
        format!("{}\n", h.join(".git").display()),
    )
    .unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_moved_worktree_whose_path_is_not_utf8_gets_no_repair_command() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let b = ws.dir("b");
    ws.add_worktree(&app, &b, &["-b", "b"]);
    let odd = ws.root().join(OsStr::from_bytes(b"b\xff"));
    std::fs::rename(&b, &odd).unwrap();
    ws.assert_head(&odd, Some("b"));
    // the path shown lossily names no dir: git refuses to repair it
    let lossy = odd.to_string_lossy().into_owned();
    ws.git_fails(&app, &["worktree", "repair", &lossy]);

    let origin = owned_origin("app");
    let blocked = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::NonUtf8Path),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [stray("b\u{fffd}", Some(&origin), true, blocked)]
    );
    // the advice holds: renamed to a UTF-8 name, the repair is offered and
    // reconnects it
    let renamed = ws.dir("b-renamed");
    std::fs::rename(&odd, &renamed).unwrap();
    assert_eq!(
        ws.unregistered(),
        [stray("b-renamed", Some(&origin), true, moved("app"))]
    );
    ws.git(&app, &["worktree", "repair", renamed.to_str().unwrap()]);
    assert_eq!(ws.unregistered(), []);
}

#[test]
fn a_copy_of_a_worktree_whose_gitfile_git_cuts_at_a_nul_shares_its_git_dir() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let wt = ws.dir("wt");
    let admin = ws.add_worktree(&app, &wt, &["-b", "wt"]);
    let copy = ws.dir("wt-copy");
    copy_dir(&ws, &wt, &copy);
    // the live one's `.git` names its git dir past a NUL: git follows it
    std::fs::write(wt.join(".git"), gitfile(b"", &admin, b"\0junk\n")).unwrap();
    ws.assert_head(&wt, Some("wt"));
    assert_eq!(
        ws.git(&wt, &["rev-parse", "--absolute-git-dir"]),
        admin.to_str().unwrap()
    );
    ws.worktree_record(&app, &wt);

    // so the copy shares it, and a repair there would take it from `wt`
    let origin = owned_origin("app");
    assert_eq!(
        ws.unregistered(),
        [stray("wt-copy", Some(&origin), true, shared("app", &wt))]
    );
}

#[test]
fn a_repair_that_would_rewrite_a_checkout_whose_path_is_not_utf8_is_not_offered() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (s_moved, _) = moved_by_hand(&ws, &app, "s");
    // `q\xff`, its `.git` gone: git's repair walk would write one there
    let q = ws.root().join(OsStr::from_bytes(b"q\xff"));
    let q_admin = add_worktree_at(&ws, &app, &q, &["-b", "q"]);
    std::fs::remove_file(q.join(".git")).unwrap();
    assert!(lists_worktree(&worktree_list(&ws, &app), &q));

    let origin = owned_origin("app");
    let blocked = UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by: Some(RepairBlock::Rewrites {
            path: q.to_string_lossy().into_owned(),
            git_dir: q_admin.to_string_lossy().into_owned(),
        }),
        exit_noise: None,
    };
    assert_eq!(
        ws.unregistered(),
        [stray("s-moved", Some(&origin), true, blocked)]
    );
    // as git does, when the repair is run anyway
    ws.git(&app, &["worktree", "repair", s_moved.to_str().unwrap()]);
    assert!(q.join(".git").is_file());
}
