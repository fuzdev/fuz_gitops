//! The unregistered scan's kinds: clones, stray worktrees, and a registered
//! repo's moved, orphaned, or copied worktrees, while live worktrees, registry
//! dirs, and dirs without a `.git` stay out.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use fuz_repos::report::UnregisteredKind;
use fuz_repos::state::Presence;
use support::unregistered::{
    app, copy_dir, gitdir_file, moved, moved_relative, shared, shared_unnamed, stray,
};
use support::{
    FixtureWorkspace, assert_git_dir_unchanged, owned_origin, seal, snapshot_git_dir,
    third_party_origin,
};

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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    assert_eq!(gitdir_file(&git_dir), feat.join(".git").to_str().unwrap());
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&feat, &moved_to).unwrap();
    // git still names the old path, and lists it as prunable
    assert_eq!(gitdir_file(&git_dir), feat.join(".git").to_str().unwrap());
    assert!(
        ws.worktree_record(&app, &feat)
            .iter()
            .any(|l| l == "prunable gitdir file points to non-existent location"),
        "{:?}",
        ws.worktree_record(&app, &feat)
    );
    // a worktree whose git dir names nothing: git doesn't list it
    let named = ws.dir("app-unnamed");
    let unnamed_git_dir = ws.add_worktree(&app, &named, &["-b", "unnamed"]);
    std::fs::remove_file(unnamed_git_dir.join("gitdir")).unwrap();
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
    let git_dir = ws.add_worktree(&app, &orphan, &["-b", "orphan"]);
    std::fs::remove_dir_all(&git_dir).unwrap();
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &feat, &copy);
    // both name one git dir, which names the original
    assert_eq!(
        std::fs::read_to_string(copy.join(".git")).unwrap(),
        std::fs::read_to_string(feat.join(".git")).unwrap()
    );
    assert_eq!(gitdir_file(&git_dir), feat.join(".git").to_str().unwrap());

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
    let git_dir = ws.add_worktree(&far, &far_feat, &["-b", "feat"]);
    assert!(!std::fs::read_to_string(git_dir.join("config")).is_ok_and(|c| c.contains("far")));
    // and an orphan of it
    let far_orphan = ws.dir("far-orphan");
    let orphan_git_dir = ws.add_worktree(&far, &far_orphan, &["-b", "orphan"]);
    std::fs::remove_dir_all(&orphan_git_dir).unwrap();

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

#[test]
fn copies_of_a_moved_worktree_share_its_git_dir_and_none_is_repairable() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    // the git dir names a path that's gone
    let feat = ws.dir("app-feat");
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let (c1, c2) = (ws.dir("app-c1"), ws.dir("app-c2"));
    copy_dir(&ws, &feat, &c1);
    copy_dir(&ws, &feat, &c2);
    std::fs::remove_dir_all(&feat).unwrap();
    assert_eq!(gitdir_file(&git_dir), feat.join(".git").to_str().unwrap());
    // the git dir names nothing
    let g = ws.dir("app-g");
    let g_git_dir = ws.add_worktree(&app, &g, &["-b", "g"]);
    let (g1, g2) = (ws.dir("app-g1"), ws.dir("app-g2"));
    copy_dir(&ws, &g, &g1);
    copy_dir(&ws, &g, &g2);
    std::fs::remove_dir_all(&g).unwrap();
    std::fs::remove_file(g_git_dir.join("gitdir")).unwrap();
    for (copy, git_dir) in [
        (&c1, &git_dir),
        (&c2, &git_dir),
        (&g1, &g_git_dir),
        (&g2, &g_git_dir),
    ] {
        assert_eq!(
            PathBuf::from(ws.git(copy, &["rev-parse", "--absolute-git-dir"])),
            *git_dir
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
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
    assert!(git_dir.join("locked").is_file());
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
    let git_dir = ws.add_worktree(&app, &bare, &["-b", "bare"]);
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    std::fs::remove_file(git_dir.join("HEAD")).unwrap();
    // `gitdir` naming this path, no `HEAD`
    let headless = ws.dir("app-headless");
    let headless_git_dir = ws.add_worktree(&app, &headless, &["-b", "headless"]);
    std::fs::remove_file(headless_git_dir.join("HEAD")).unwrap();
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
    let git_dir = ws.add_worktree(&app, &wt, &["-b", "feat"]);
    // its git dir moved out of `worktrees/`, still naming this path, its
    // `commondir` pointing back: git works in it but doesn't list it
    let hidden = ws.outside("hidden");
    std::fs::rename(&git_dir, &hidden).unwrap();
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let copy = ws.dir("app-copy");
    copy_dir(&ws, &feat, &copy);
    assert_eq!(
        PathBuf::from(ws.git(&copy, &["rev-parse", "--absolute-git-dir"])),
        git_dir
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
    let git_dir = ws.add_worktree(app, &feat, &["-b", "feat"]);
    copy_dir(ws, &feat, &ws.dir("app-copy"));
    (feat, git_dir)
}

#[test]
fn a_locked_worktree_is_never_moved_whatever_its_gitdir_holds() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (feat, git_dir) = worktree_on_usb(&ws, &app);
    ws.git(&app, &["worktree", "lock", feat.to_str().unwrap()]);
    // its `gitdir` lost, and its media unmounted
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    std::fs::rename(ws.outside("usb"), ws.outside("usb-unmounted")).unwrap();
    assert!(git_dir.join("locked").is_file() && git_dir.join("HEAD").is_file());
    // git keeps the git dir: a lock stops prune
    ws.git(&app, &["worktree", "prune"]);
    assert!(git_dir.is_dir());

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
    std::fs::write(git_dir.join("gitdir"), "").unwrap();
    assert_eq!(ws.unregistered()[0].kind, shared_unnamed("app"));
    // unlocked (git can't, it no longer lists the worktree), a lost
    // `gitdir` is repair's to rewrite
    ws.git_fails(&app, &["worktree", "unlock", feat.to_str().unwrap()]);
    std::fs::remove_file(git_dir.join("locked")).unwrap();
    assert_eq!(ws.unregistered()[0].kind, moved("app"));
}

#[test]
fn a_gitdir_that_cannot_be_read_may_name_a_worktree_in_use() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let (feat, git_dir) = worktree_on_usb(&ws, &app);
    // the original is live: git works in it
    let Some(_sealed) = seal(&git_dir.join("gitdir"), 0o000) else {
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    let link = ws.dir("link");
    std::fs::create_dir(&link).unwrap();
    symlink(&git_dir, link.join(".git")).unwrap();
    // a moved worktree, and a `.git` linking into its git dir: a repair of
    // either would take it from the other
    let gone = ws.dir("app-gone");
    let gone_git_dir = ws.add_worktree(&app, &gone, &["-b", "gone"]);
    let moved_to = ws.dir("app-moved");
    std::fs::rename(&gone, &moved_to).unwrap();
    let gone_link = ws.dir("gone-link");
    std::fs::create_dir(&gone_link).unwrap();
    symlink(&gone_git_dir, gone_link.join(".git")).unwrap();
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
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    std::fs::write(git_dir.join("gitdir"), "../../../../app-feat/.git\n").unwrap();
    std::fs::write(
        feat.join(".git"),
        "gitdir: ../app/.git/worktrees/app-feat\n",
    )
    .unwrap();
    assert_eq!(
        git_dir
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
            moved_relative("app", &git_dir)
        )]
    );
}

#[test]
fn a_worktree_git_dir_whose_commondir_cannot_be_read() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let feat = ws.outside("app-feat");
    let git_dir = ws.add_worktree(&app, &feat, &["-b", "feat"]);
    copy_dir(&ws, &feat, &ws.dir("app-copy"));
    let Some(_sealed) = seal(&git_dir.join("commondir"), 0o000) else {
        return;
    };

    assert_eq!(
        ws.unregistered(),
        [stray("app-copy", None, false, UnregisteredKind::Worktree)]
    );
}
