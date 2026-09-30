use super::*;
use crate::registry::Registry;

#[test]
fn refspecs_writing_outside_remote_tracking_refs() {
    for refspec in [
        "+refs/tags/*:refs/tags/*",
        "+refs/*:refs/*",
        "+refs/heads/*:refs/heads/*",
        "refs/heads/main:refs/heads/main",
        "main:local",
        "+refs/heads/x:refs/bundles/x",
        // another remote's namespace, or all of them
        "+refs/heads/*:refs/remotes/*",
        "+refs/heads/*:refs/remotes/upstream/*",
        "+refs/heads/*:refs/remotes/origin2/*",
        // a shorthand destination: refused as written
        "+refs/heads/*:remotes/origin/*",
    ] {
        assert!(refspec_writes_outside_origin(refspec), "{refspec}");
    }
    for refspec in [
        "+refs/heads/*:refs/remotes/origin/*",
        "refs/heads/main:refs/remotes/origin/main",
        // a narrowed single-branch clone's
        "+refs/heads/feat:refs/remotes/origin/feat",
        "+refs/pull/*/head:refs/remotes/origin/pr/*",
        // `FETCH_HEAD` alone
        "feat",
        "refs/heads/feat",
        "refs/heads/feat:",
        // negative: writes nothing
        "^refs/heads/wip",
    ] {
        assert!(!refspec_writes_outside_origin(refspec), "{refspec}");
    }
}

#[test]
fn other_remotes_writing_into_origins_namespace() {
    for refspec in [
        "+refs/heads/*:refs/remotes/origin/fork/*",
        "refs/heads/main:refs/remotes/origin/main",
        "+refs/heads/*:remotes/origin/fork/*",
        // a `*` that can expand across the `/`
        "+refs/heads*:refs/remotes/origin*",
        "+refs/heads/*:refs/remotes/*",
        "+refs/heads/*:refs/remotes/orig*",
        "+refs/*:refs/*",
        "+refs/remotes/*:refs/remotes/*",
        "+refs/heads/*:remotes/*",
        // full-name globs: substituted as written, no DWIM
        "+ref*:ref*",
        "+refs*:refs*",
        "+*:*",
        "+r*:r*",
        "+refs/heads/*:*",
        // a `*` mid-path: a branch `rigin` lands at `origin/x`
        "+refs/heads/*:refs/remotes/o*/x",
        // git ignores these (funny refs); refused anyway
        "+refs/heads/*:remotes/origin/*",
        // a non-`*` shorthand git DWIMs under `refs/`
        "refs/heads/fx:remotes/origin/fx",
        // case-folded, for case-insensitive file systems
        "+refs/heads/*:refs/remotes/ORIGIN/*",
        "refs/heads/fx:Refs/Remotes/Origin/fx",
    ] {
        assert!(refspec_writes_into_origin(refspec), "{refspec}");
    }
    // what a legacy `Pull:` line carries is the same refspec syntax
    for refspec in ["+ref*:ref*", "+refs*:refs*"] {
        assert!(refspec_writes_into_origin(refspec), "{refspec}");
    }
    for refspec in [
        // what an `upstream` remote carries in the real workspace
        "+refs/heads/*:refs/remotes/upstream/*",
        "+refs/heads/*:refs/remotes/origin2/*",
        "+refs/heads/*:refs/remotes/originals/*",
        "+refs/heads/*:refs/remotes/up*",
        "refs/heads/main:refs/remotes/origin",
        "+refs/tags/*:refs/tags/*",
        "+refs/heads/*:tags*",
        "+refs/heads/*:refs/remotes/x*",
        // git DWIMs a bare name under `refs/heads/`
        "refs/heads/fx:origin/fx",
        "refs/heads/fx:heads/fx2",
        "feat",
        "^refs/remotes/origin/x",
    ] {
        assert!(!refspec_writes_into_origin(refspec), "{refspec}");
    }
}

#[test]
fn origins_own_globs_stay_in_its_namespace() {
    assert!(!refspec_writes_outside_origin(
        "+refs/heads/*:refs/remotes/origin/*"
    ));
    for refspec in [
        "+refs/heads/*:refs/remotes/origin*",
        "+refs/heads/*:refs/remotes/*",
        "+refs/*:refs/*",
    ] {
        assert!(refspec_writes_outside_origin(refspec), "{refspec}");
    }
}

#[test]
fn legacy_remotes_writing_into_origin_are_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path();
    assert_eq!(legacy_remote_refusal(common), None);
    let remotes = common.join("remotes");
    std::fs::create_dir(&remotes).unwrap();
    std::fs::write(
        remotes.join("fine"),
        "URL: file:///x\nPull: +refs/heads/*:refs/remotes/fine/*\n",
    )
    .unwrap();
    // a dir there is no remote
    std::fs::create_dir(remotes.join("adir")).unwrap();
    // nor is a legacy `origin`: git reads it only when config gives origin
    // no URL, and then there's no fetch
    std::fs::write(
        remotes.join("origin"),
        "URL: file:///x\nPull: +refs/heads/*:refs/remotes/origin/*\n",
    )
    .unwrap();
    // a non-UTF-8 byte doesn't make a harmless file unreadable
    std::fs::write(
        remotes.join("bytes"),
        b"URL: file:///\xff\nPull: +refs/heads/*:refs/remotes/b\xffs/*\n",
    )
    .unwrap();
    assert_eq!(legacy_remote_refusal(common), None);
    std::fs::write(
        remotes.join("legacy"),
        "URL: file:///x\nPull:  +refs/heads/fx:refs/remotes/origin/fork-fx\n",
    )
    .unwrap();
    assert_eq!(
        legacy_remote_refusal(common),
        Some(RemoteFailure::OriginRefsShared {
            remote: "legacy".into(),
            refspec: "+refs/heads/fx:refs/remotes/origin/fork-fx".into(),
        })
    );
    // fails closed on what can't be read: a file, then the dir itself
    std::fs::remove_file(remotes.join("legacy")).unwrap();
    let sealed = remotes.join("sealed");
    std::fs::write(&sealed, "Pull: +refs/heads/*:refs/remotes/fine/*\n").unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&sealed).is_ok() {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    assert_eq!(
        legacy_remote_refusal(common),
        Some(RemoteFailure::LegacyRemotesUnreadable {
            path: sealed.to_string_lossy().into_owned()
        })
    );
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&remotes, std::fs::Permissions::from_mode(0o000)).unwrap();
    let refusal = legacy_remote_refusal(common);
    std::fs::set_permissions(&remotes, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        refusal,
        Some(RemoteFailure::LegacyRemotesUnreadable {
            path: remotes.to_string_lossy().into_owned()
        })
    );
}

fn r(upstream: Option<&str>, track: Track) -> RefFacts {
    RefFacts {
        name: "b".into(),
        oid: "c".into(),
        symref: None,
        upstream_ref: upstream.map(str::to_owned),
        merge_ref: upstream.map(|_| "refs/heads/b".to_owned()),
        track,
        worktree: None,
        committer_time: 0,
    }
}

#[test]
fn local_work_is_possible_off_an_even_or_behind_upstream() {
    let up = Some("refs/remotes/origin/b");
    assert!(!could_carry_local_work(&r(up, Track::Even)));
    assert!(!could_carry_local_work(&r(up, Track::Behind(3))));
    assert!(could_carry_local_work(&r(up, Track::Ahead(1))));
    assert!(could_carry_local_work(&r(
        up,
        Track::Diverged {
            ahead: 1,
            behind: 1
        }
    )));
    assert!(could_carry_local_work(&r(up, Track::Gone)));
    assert!(could_carry_local_work(&r(None, Track::Even)));
    // an alias carries nothing of its own
    let alias = RefFacts {
        symref: Some("refs/heads/main".into()),
        ..r(
            None,
            Track::Diverged {
                ahead: 1,
                behind: 1,
            },
        )
    };
    assert!(!could_carry_local_work(&alias));
}

#[test]
fn only_a_clone_entry_dates_a_clone() {
    let zero = "0".repeat(40);
    let oid = "1f8d9cf7b1e352f1b0c1a76e159204b8eedf07c7";
    let line = |rest: &str| format!("{zero} {oid} A U Thor <a@example.com> {rest}");
    for (rest, want) in [
        (
            "1790735590 -0400\tclone: from git@github.com:me/app",
            Some(1_790_735_590),
        ),
        (
            "1790735590 +0000\tclone: from /a path/with spaces",
            Some(1_790_735_590),
        ),
        (
            "1790735590 +0000\tclone: from https://h/a>b",
            Some(1_790_735_590),
        ),
        // another entry, or a message that only mentions a clone
        ("1790735590 +0000\tcheckout: moving from main to dev", None),
        ("1790735590 +0000\tbranch: clone: x", None),
        ("1790735590 +0000\tclone:from x", None),
        ("1790735590 +0000\t", None),
        ("1790735590 +0000", None),
        // a time or zone git wouldn't have written
        ("-1 +0000\tclone: from x", None),
        ("1790735590 0000\tclone: from x", None),
        ("1790735590 +00\tclone: from x", None),
        ("1790735590 x0000\tclone: from x", None),
        ("1790735590 +00a0\tclone: from x", None),
        ("1790735590\tclone: from x", None),
        ("x1790735590 +0000\tclone: from x", None),
        (" 1790735590 +0000\tclone: from x", None),
        ("99999999999999999999999 +0000\tclone: from x", None),
    ] {
        assert_eq!(parse_clone_entry(line(rest).as_bytes()), want, "{rest}");
    }
    // an ident with no email's end
    let bare = format!("{zero} {oid} A U Thor 1790735590 +0000\tclone: from x");
    assert_eq!(parse_clone_entry(bare.as_bytes()), None);
    // a tab in the name is the ident's, not the message's
    let tabbed = format!("{zero} {oid} A\tU <a@b> 1790735590 +0000\tclone: from x");
    assert_eq!(parse_clone_entry(tabbed.as_bytes()), Some(1_790_735_590));
}

#[test]
fn a_clone_time_is_read_from_the_first_line_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let logs = tmp.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    assert_eq!(clone_time(tmp.path()), None);
    let entry = |time: u64, message: &str| {
        format!(
            "{} {} A <a@b> {time} +0000\t{message}\n",
            "0".repeat(40),
            "1".repeat(40)
        )
    };
    let clone = entry(1_700_000_000, "clone: from x");
    let checkout = entry(1_700_000_060, "checkout: moving from main to dev");
    std::fs::write(logs.join("HEAD"), format!("{clone}{checkout}")).unwrap();
    assert_eq!(clone_time(tmp.path()), Some(1_700_000_000));
    std::fs::write(logs.join("HEAD"), format!("{checkout}{clone}")).unwrap();
    assert_eq!(clone_time(tmp.path()), None);
    // a line git never finished, or one past the limit
    std::fs::write(logs.join("HEAD"), clone.trim_end()).unwrap();
    assert_eq!(clone_time(tmp.path()), None);
    let long = entry(
        1_700_000_000,
        &format!("clone: from {}", "x".repeat(70_000)),
    );
    std::fs::write(logs.join("HEAD"), long).unwrap();
    assert_eq!(clone_time(tmp.path()), None);
    std::fs::write(logs.join("HEAD"), "").unwrap();
    assert_eq!(clone_time(tmp.path()), None);
}

#[test]
fn in_progress_ignores_a_stale_rebase_head() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("REBASE_HEAD"), "abc\n").unwrap();
    assert_eq!(read_in_progress(tmp.path()).unwrap(), None);
    std::fs::create_dir(tmp.path().join("rebase-merge")).unwrap();
    assert_eq!(
        read_in_progress(tmp.path()).unwrap(),
        Some(InProgressOp::Rebase)
    );
}

#[test]
fn in_progress_tells_am_from_an_apply_rebase() {
    let tmp = tempfile::tempdir().unwrap();
    let apply = tmp.path().join("rebase-apply");
    std::fs::create_dir(&apply).unwrap();
    assert_eq!(
        read_in_progress(tmp.path()).unwrap(),
        Some(InProgressOp::Rebase)
    );
    std::fs::write(apply.join("applying"), "").unwrap();
    assert_eq!(
        read_in_progress(tmp.path()).unwrap(),
        Some(InProgressOp::Am)
    );
}

#[test]
fn an_unlisted_worktree_takes_its_path_from_a_readable_gitdir() {
    let tmp = tempfile::tempdir().unwrap();
    let admin = tmp.path().join("wt");
    std::fs::create_dir(&admin).unwrap();
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/feat\n").unwrap();
    std::fs::create_dir(admin.join("rebase-merge")).unwrap();
    let mut unreadable = Vec::new();
    // named by a readable `gitdir`: its worktree's path
    let named = AdminDir {
        dir: admin.clone(),
        worktree: Ok(PathBuf::from("/ws/app-feat")),
        relative: false,
        unreadable: false,
    };
    let u = unlisted_worktree(&named, &mut unreadable);
    assert_eq!(u.path, "/ws/app-feat");
    assert_eq!(
        u.why,
        UnprobedWhy::Failed {
            error: format!("not listed by git: its git dir is {}", admin.display())
        }
    );
    assert_eq!(
        u.head,
        UnprobedHead::Branch {
            name: "feat".into()
        }
    );
    assert_eq!(u.in_progress, Some(InProgressOp::Rebase));
    // not: the git dir itself
    let unnamed = AdminDir {
        dir: admin.clone(),
        worktree: Err("gone".into()),
        relative: false,
        unreadable: false,
    };
    let u = unlisted_worktree(&unnamed, &mut unreadable);
    assert_eq!(u.path, admin.to_str().unwrap());
    assert_eq!(
        u.why,
        UnprobedWhy::Failed {
            error: "not listed by git: gone".into()
        }
    );
    assert!(unreadable.is_empty());
}

#[test]
fn a_failed_record_takes_its_head_from_its_git_dir() {
    // git read the HEAD for its list, and it changed since: the git dir
    // is what's there now
    let tmp = tempfile::tempdir().unwrap();
    let admin = tmp.path().join("admin");
    std::fs::create_dir(&admin).unwrap();
    std::fs::write(admin.join("HEAD"), "garbage\n").unwrap();
    let record = WorktreeRecord {
        // gone: probed without a git call
        path: tmp.path().join("gone").to_string_lossy().into_owned(),
        head: WorktreeHead::Detached {
            commit: "3890426260f93bbbdf34262c865872c38302836e".into(),
        },
        locked: Some(String::new()),
        prunable: None,
    };
    let git = Git::new();
    let mut w = Worktrees::default();
    probe_record(&git, record, Some(&admin), true, &HashSet::new(), &mut w);
    assert_eq!(git.spawns(), 0);
    assert_eq!(w.unprobed.len(), 1);
    assert_eq!(w.unprobed[0].head, UnprobedHead::Unknown);
    assert_eq!(w.unprobed[0].why, UnprobedWhy::Missing);
}

#[test]
fn submodules_that_cannot_be_looked_at_count() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let sealed = tmp.path().join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = sealed.join("x").try_exists().is_err();
    let result = (
        any_populated(tmp.path(), &["sealed/nested".to_owned()]),
        any_populated(tmp.path(), &["absent".to_owned()]),
        // a git dir whose `modules/` can't be checked, and no index read
        submodule_refusal(&Git::new(), tmp.path(), &sealed.join("admin"), false),
    );
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !unreadable {
        eprintln!("skipped: permissions don't bind this user (root)");
        return;
    }
    assert_eq!(result, (true, false, Some(true)));
}

/// A runner that sees no global or system config.
fn hermetic_git() -> Git {
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ];
    env.extend(std::env::var_os("PATH").map(|p| ("PATH".into(), p)));
    Git::with_clean_env(env)
}

/// `git init` plus a gitlink named `name` in the index, never populated.
fn repo_with_gitlink(dir: &Path, name: &std::ffi::OsStr) {
    let git = |args: &[&std::ffi::OsStr]| {
        let status = std::process::Command::new("git")
            .env_clear()
            .envs(std::env::var_os("PATH").map(|p| ("PATH", p)))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    };
    std::fs::create_dir(dir).unwrap();
    git(&["init".as_ref(), "-q".as_ref()]);
    let mut info = std::ffi::OsString::from("160000,3890426260f93bbbdf34262c865872c38302836e,");
    info.push(name);
    git(&[
        "update-index".as_ref(),
        "--add".as_ref(),
        "--cacheinfo".as_ref(),
        info.as_os_str(),
    ]);
}

#[test]
fn a_gitlink_list_that_cannot_be_read_refuses() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let git = hermetic_git();
    // not a repo: `ls-files` fails
    let plain = tmp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert_eq!(
        submodule_refusal(&git, &plain, &plain.join(".git"), true),
        Some(true)
    );
    // a gitlink whose name isn't UTF-8: the list can't be parsed
    let odd = tmp.path().join("odd");
    repo_with_gitlink(&odd, std::ffi::OsStr::from_bytes(b"sub\xff"));
    assert_eq!(
        submodule_refusal(&git, &odd, &odd.join(".git"), true),
        Some(true)
    );
    // control: a UTF-8 gitlink, never populated, doesn't refuse
    let fine = tmp.path().join("fine");
    repo_with_gitlink(&fine, "sub".as_ref());
    assert_eq!(
        submodule_refusal(&git, &fine, &fine.join(".git"), true),
        Some(false)
    );
}

#[test]
fn not_a_repo_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Registry::parse(
        r#"
owners = ["me"]
[repos.empty]
url = "https://github.com/me/empty"
visibility = "public"
purpose = "a clone that never started"
[repos.plain]
url = "https://github.com/me/plain"
visibility = "public"
purpose = "a dir that isn't a checkout"
[repos.stub]
url = "https://github.com/me/stub"
visibility = "public"
purpose = "a .git git can't use"
"#,
    )
    .unwrap()
    .validate()
    .unwrap();
    std::fs::create_dir(tmp.path().join("empty")).unwrap();
    std::fs::create_dir(tmp.path().join("plain")).unwrap();
    std::fs::write(tmp.path().join("plain/file"), "x").unwrap();
    std::fs::create_dir_all(tmp.path().join("stub/.git")).unwrap();
    let git = Git::new();
    let registry_dirs = RegistryDirs::default();
    let cx = ProbeContext {
        git: &git,
        root: tmp.path(),
        registry_dirs: &registry_dirs,
        fetch: false,
        refresh: Refresh::Unasked,
        fetches: &RepoFetches::default(),
    };
    let details: Vec<String> = registry
        .entries()
        .iter()
        .map(|e| match probe(e, cx).probed {
            Probed::NotARepo { detail } => detail,
            p => panic!("{}: {p:?}", e.key),
        })
        .collect();
    assert_eq!(details[0], "empty directory");
    assert_eq!(details[1], "no .git: a copy of the files, not a clone");
    assert!(
        details[2].contains("not a git repository"),
        "{}",
        details[2]
    );
}
