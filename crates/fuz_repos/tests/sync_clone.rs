//! `repos sync` cloning missing entries over fixture workspaces: the recipe
//! per entry kind (owned over the fixture's `ssh`, third-party over its
//! `https`), what a clone leaves (exact refs, config, and files), what never
//! gets cloned over, what holds a clone, and failures that leave nothing at
//! the entry's path.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::clone::temp_dir_name;
use fuz_repos::remote::RemoteFailure;
use fuz_repos::report::{CloneOutcome, EntrySync, FetchOutcome, SyncHold, UnregisteredKind};
use fuz_repos::sessions::{Caller, LiveSessions, Session, SessionSource, Unavailable};
use fuz_repos::state::{CloneRecipe, CloneVerdict, HeldBy, Presence, Verdict};
use fuz_repos::sync::SyncRun;
use support::{FixtureWorkspace, LiveChild, OWNER, THIRD_PARTY, find_entry, write};

const fn quiet() -> LiveSessions {
    LiveSessions::Known(Vec::new())
}

fn outcomes<'a>(run: &'a SyncRun, key: &str) -> &'a EntrySync {
    run.outcomes
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no outcomes for {key}: {:?}", run.outcomes))
}

fn cloned(run: &SyncRun, key: &str) -> CloneOutcome {
    outcomes(run, key)
        .clone
        .clone()
        .unwrap_or_else(|| panic!("no clone outcome for {key}: {:?}", run.outcomes))
}

/// The workspace root's entries, sorted: what a run left there.
fn root_listing(ws: &FixtureWorkspace) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(ws.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// The files of a checkout, `.git` aside, relative and sorted.
fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The repo's local config, one `key=value` per line, sorted.
fn local_config(ws: &FixtureWorkspace, repo: &Path) -> Vec<String> {
    let mut lines: Vec<String> = ws
        .git_raw(repo, &["config", "--local", "--list"])
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// A bare remote's refs, `refs/heads/<b>` by name.
fn remote_heads(ws: &FixtureWorkspace, name: &str) -> BTreeMap<String, String> {
    ws.git_raw(
        &ws.bare(name),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
    )
    .lines()
    .map(|l| {
        let (r, oid) = l.split_once(' ').unwrap();
        (r.to_owned(), oid.to_owned())
    })
    .collect()
}

/// A live session of `child` recorded at `cwd`, as the reader vouches for it.
fn session(child: &LiveChild, cwd: &Path) -> Session {
    Session::at(
        child.pid(),
        child.proc_start().parse().unwrap(),
        cwd.to_str().unwrap().to_owned(),
        SessionSource::SessionFile,
    )
}

/// The clone verdict status gives an entry.
fn verdict(ws: &FixtureWorkspace, key: &str) -> Option<CloneVerdict> {
    ws.entry(key).clone
}

/// A user template dir whose hooks would each record that they ran, set
/// for every later call as `init.templateDir`, the way a user's global
/// config would; returns where a hook's run is recorded.
fn hooked_template(ws: &mut FixtureWorkspace) -> std::path::PathBuf {
    let template = ws.outside("template");
    let ran = ws.outside("hook-ran");
    for hook in [
        "post-checkout",
        "reference-transaction",
        "post-index-change",
    ] {
        support::write_executable(
            &template,
            &format!("hooks/{hook}"),
            &format!("#!/bin/sh\necho {hook} >> '{}'\n", ran.display()),
        );
    }
    ws.set_env("GIT_CONFIG_COUNT", "1");
    ws.set_env("GIT_CONFIG_KEY_0", "init.templateDir");
    ws.set_env("GIT_CONFIG_VALUE_0", template.as_os_str().to_owned());
    ran
}

#[test]
fn an_owned_entry_is_cloned_over_ssh_on_its_branch() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("src/lib.rs", "fn main() {}\n")]);
    ws.upstream_commit("app", "dev");
    let up = ws.upstream("app");
    ws.git(&up, &["tag", "v1"]);
    ws.git(&up, &["push", "-q", "origin", "v1"]);
    ws.declare_repo("app", "app", "branch = \"dev\"");
    ws.write_registry();
    let heads = remote_heads(&ws, "app");
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(ws.has_ref(&ws.bare("app"), "refs/tags/v1"));
    let ran = hooked_template(&mut ws);
    assert!(!ws.dir("app").exists());

    // the preview: the recipe, decided in classify
    assert_eq!(
        verdict(&ws, "app"),
        Some(CloneVerdict::Act {
            recipe: CloneRecipe {
                url: format!("git@github.com:{OWNER}/app"),
                branch: Some("dev".into()),
                shallow: false,
                sparse: None,
            }
        })
    );

    let run = ws.sync();

    let app = ws.dir("app");
    // the user's template came along, as with any clone, and no hook ran;
    // dropped before the fixture's own git calls, which would run them
    let mut hooks: Vec<String> = std::fs::read_dir(app.join(".git/hooks"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    hooks.sort();
    assert_eq!(
        hooks,
        [
            "post-checkout",
            "post-index-change",
            "reference-transaction"
        ]
    );
    assert!(!ran.exists(), "a hook ran");
    std::fs::remove_dir_all(app.join(".git/hooks")).unwrap();
    let dev = heads["refs/heads/dev"].clone();
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Cloned {
            branch: "dev".into(),
            head: dev.clone(),
        }
    );
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::NotFetched);
    assert!(outcomes(&run, "app").branches.is_empty());
    // over SSH, from the registry's repo
    assert_eq!(ws.ssh_log().len(), 1, "{:?}", ws.ssh_log());
    assert!(
        ws.ssh_log()[0].ends_with(&format!("git@github.com git-upload-pack '{OWNER}/app'")),
        "{:?}",
        ws.ssh_log()
    );
    // exactly: on dev, every branch mapped, no tag
    let mut want: BTreeMap<String, String> = BTreeMap::new();
    want.insert("HEAD".into(), "refs/heads/dev".into());
    want.insert("refs/heads/dev".into(), dev);
    for (r, oid) in &heads {
        let b = r.strip_prefix("refs/heads/").unwrap();
        want.insert(format!("refs/remotes/origin/{b}"), oid.clone());
    }
    want.insert(
        "refs/remotes/origin/HEAD".into(),
        heads["refs/heads/main"].clone(),
    );
    assert_eq!(ws.refs(&app), want);
    assert_eq!(
        local_config(&ws, &app),
        [
            "branch.dev.merge=refs/heads/dev",
            "branch.dev.remote=origin",
            "core.bare=false",
            "core.filemode=true",
            "core.logallrefupdates=true",
            "core.repositoryformatversion=0",
            "remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*",
            &format!("remote.origin.url=git@github.com:{OWNER}/app"),
        ]
    );
    ws.assert_head(&app, Some("dev"));
    ws.assert_upstream(&app, "dev", "refs/remotes/origin/dev");
    ws.assert_clean(&app);
    assert_eq!(files(&app), ["README", "src/lib.rs", "upstream-dev.txt"]);
    // nothing else at the root: no temp dir left
    assert_eq!(root_listing(&ws), ["app", "repos.toml"]);

    // status reads it present and in sync, origin as the registry says
    let e = ws.entry("app");
    assert_eq!(e.presence, Presence::Present);
    assert_eq!(e.clone, None);
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(
        e.branches.iter().all(|b| b.verdict == Verdict::Quiet),
        "{:?}",
        e.branches
    );
    // a second sync has nothing to clone, and fetches it instead — no tags
    let run = ws.sync();
    assert_eq!(outcomes(&run, "app").clone, None);
    assert_eq!(outcomes(&run, "app").fetch, FetchOutcome::Fetched);
    assert!(!ws.has_ref(&app, "refs/tags/v1"));
    // the user's own fetch follows tags, as in any clone
    ws.git(&app, &["fetch", "-q"]);
    assert!(ws.has_ref(&app, "refs/tags/v1"));
}

#[test]
fn a_third_party_reference_is_cloned_over_https_from_the_remote_default() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.upstream_commit("lib", "other");
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    ws.serve_https();
    ws.write_registry();
    let heads = remote_heads(&ws, "lib");
    assert_eq!(
        ws.git(&ws.bare("lib"), &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );

    assert_eq!(
        verdict(&ws, "lib"),
        Some(CloneVerdict::Act {
            recipe: CloneRecipe {
                url: format!("https://github.com/{THIRD_PARTY}/lib"),
                branch: None,
                shallow: false,
                sparse: None,
            }
        })
    );
    let run = ws.sync();

    let lib = ws.dir("lib");
    assert_eq!(
        cloned(&run, "lib"),
        CloneOutcome::Cloned {
            branch: "main".into(),
            head: heads["refs/heads/main"].clone(),
        }
    );
    // over HTTPS alone
    assert_eq!(
        ws.https_log(),
        [format!("https://github.com/{THIRD_PARTY}/lib")]
    );
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    ws.assert_head(&lib, Some("main"));
    ws.assert_upstream(&lib, "main", "refs/remotes/origin/main");
    assert_eq!(
        ws.git(&lib, &["config", "remote.origin.url"]),
        format!("https://github.com/{THIRD_PARTY}/lib")
    );
    ws.assert_clean(&lib);
    assert_eq!(root_listing(&ws), ["lib", "repos.toml"]);
    let e = ws.entry("lib");
    assert_eq!(e.presence, Presence::Present);
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
}

#[test]
fn a_shallow_reference_is_cloned_at_depth_one_on_its_branch_alone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("spec", &[]);
    ws.upstream_commit("spec", "main");
    ws.upstream_commit("spec", "fork");
    let fork = ws.upstream_commit("spec", "fork");
    ws.declare_reference(
        "spec",
        THIRD_PARTY,
        "spec",
        "branch = \"fork\"\nshallow = true",
    );
    ws.serve_https();
    ws.write_registry();
    ws.assert_count(&ws.bare("spec"), &["fork"], 4);

    let run = ws.sync();

    let spec = ws.dir("spec");
    assert_eq!(
        cloned(&run, "spec"),
        CloneOutcome::Cloned {
            branch: "fork".into(),
            head: fork.clone(),
        }
    );
    ws.assert_shallow(&spec, true);
    ws.assert_count(&spec, &["HEAD"], 1);
    // only the branch cloned is mapped
    assert_eq!(
        ws.git(&spec, &["config", "--get-all", "remote.origin.fetch"]),
        "+refs/heads/fork:refs/remotes/origin/fork"
    );
    let mut want = BTreeMap::new();
    want.insert("HEAD".to_owned(), "refs/heads/fork".to_owned());
    want.insert("refs/heads/fork".to_owned(), fork.clone());
    want.insert("refs/remotes/origin/fork".to_owned(), fork);
    assert_eq!(ws.refs(&spec), want);
    let e = ws.entry("spec");
    assert!(e.layout.as_ref().is_some_and(|l| l.shallow));
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
}

/// wpt's recipe: an owned fork, shallow, sparse, on its `fork` branch, and
/// pinned — cloned, then left to its consumer.
#[test]
fn a_sparse_pin_is_cloned_to_its_cone_alone_then_held() {
    let mut ws = FixtureWorkspace::new();
    ws.remote(
        "wpt",
        &[
            ("css/a.css", "a {}\n"),
            ("css/deep/b.css", "b {}\n"),
            ("html/c.html", "<p>\n"),
            ("top.txt", "top\n"),
        ],
    );
    ws.upstream_commit("wpt", "fork");
    ws.declare_reference(
        "wpt",
        OWNER,
        "wpt",
        "branch = \"fork\"\nshallow = true\nsparse = \"css\"\npinned = true",
    );
    ws.write_registry();
    assert_eq!(
        verdict(&ws, "wpt"),
        Some(CloneVerdict::Act {
            recipe: CloneRecipe {
                url: format!("git@github.com:{OWNER}/wpt"),
                branch: Some("fork".into()),
                shallow: true,
                sparse: Some("css".into()),
            }
        })
    );

    let run = ws.sync();

    let wpt = ws.dir("wpt");
    let fork = ws.git(&ws.bare("wpt"), &["rev-parse", "fork"]);
    assert_eq!(
        cloned(&run, "wpt"),
        CloneOutcome::Cloned {
            branch: "fork".into(),
            head: fork,
        }
    );
    // the cone and the top-level files, nothing else ever checked out
    assert_eq!(
        files(&wpt),
        [
            "README",
            "css/a.css",
            "css/deep/b.css",
            "top.txt",
            "upstream-fork.txt"
        ]
    );
    assert_eq!(
        ws.git(&wpt, &["config", "remote.origin.partialclonefilter"]),
        "blob:none"
    );
    assert_eq!(ws.git(&wpt, &["config", "core.sparseCheckoutCone"]), "true");
    assert_eq!(ws.git(&wpt, &["sparse-checkout", "list"]), "css");
    // the blob outside the cone was never fetched
    let objects = ws.git(&wpt, &["rev-list", "--objects", "--all", "--missing=print"]);
    assert_eq!(
        objects.lines().filter(|l| l.starts_with('?')).count(),
        1,
        "{objects}"
    );
    ws.assert_shallow(&wpt, true);
    ws.assert_clean(&wpt);
    // the pin: never fetched from here on
    let run = ws.sync();
    assert_eq!(outcomes(&run, "wpt").fetch, FetchOutcome::NotFetched);
    assert_eq!(outcomes(&run, "wpt").clone, None);
    let e = find_entry(&run.entries, "wpt");
    assert!(e.pinned);
    assert!(e.layout.as_ref().is_some_and(|l| l.sparse && l.shallow));
}

#[test]
fn a_failed_clone_leaves_nothing_at_the_path() {
    let mut ws = FixtureWorkspace::new();
    // no remote for either
    ws.declare_repo("gone", "gone", "");
    ws.declare_reference("lost", THIRD_PARTY, "lost", "");
    // one whose branch the remote doesn't have
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "branch = \"nope\"");
    ws.serve_https();
    ws.write_registry();

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "gone"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::RepoNotFound {
                message: "ERROR: Repository not found.".into()
            }
        }
    );
    assert_eq!(
        cloned(&run, "lost"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::RepoNotFound {
                message: "remote: Repository not found.".into()
            }
        }
    );
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::Failed {
                message: "fatal: Remote branch nope not found in upstream origin".into()
            }
        }
    );
    assert_eq!(root_listing(&ws), ["repos.toml"]);
}

/// An empty remote clones to an unborn branch, which the read back fails —
/// the clone stays where it is, for a person to look at.
#[test]
fn a_clone_that_reads_back_wrong_fails_and_stays() {
    let mut ws = FixtureWorkspace::new();
    let bare = ws.bare("empty");
    ws.git(
        ws.base(),
        &[
            "-c",
            "init.defaultBranch=main",
            "init",
            "-q",
            "--bare",
            bare.to_str().unwrap(),
        ],
    );
    assert!(!ws.has_ref(&bare, "HEAD"));
    ws.declare_reference("empty", THIRD_PARTY, "empty", "");
    ws.serve_https();
    ws.write_registry();
    let empty = ws.dir("empty");

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "empty"),
        CloneOutcome::Failed {
            message: format!("cloned into {}, but main has no commit", empty.display())
        }
    );
    ws.assert_head(&empty, Some("main"));
    assert!(!ws.has_ref(&empty, "HEAD"));
    assert_eq!(root_listing(&ws), ["empty", "repos.toml"]);
}

#[test]
fn a_timed_out_clone_leaves_nothing_at_the_path() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("slow", &[]);
    ws.stall_remote("slow");
    ws.declare_repo("slow", "slow", "");
    ws.write_registry();

    let run = ws.sync_timed(Caller::Person, 4, &quiet, Duration::from_secs(1));

    assert_eq!(
        cloned(&run, "slow"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::TimedOut { after_secs: 1 }
        }
    );
    assert_eq!(root_listing(&ws), ["repos.toml"]);
}

/// A path that holds anything is never missing, so never cloned: an empty
/// dir, a file, a dangling symlink (a clone would go through it), a
/// symlink to an empty dir.
#[test]
fn anything_at_the_path_is_never_cloned_over() {
    let mut ws = FixtureWorkspace::new();
    for name in ["empty", "file", "dangling", "linked"] {
        ws.remote(name, &[]);
        ws.declare_repo(name, name, "");
    }
    ws.write_registry();
    std::fs::create_dir(ws.dir("empty")).unwrap();
    std::fs::write(ws.dir("file"), "mine\n").unwrap();
    std::os::unix::fs::symlink(ws.outside("nowhere"), ws.dir("dangling")).unwrap();
    std::fs::create_dir(ws.outside("target")).unwrap();
    std::os::unix::fs::symlink(ws.outside("target"), ws.dir("linked")).unwrap();
    assert!(!ws.outside("nowhere").exists());

    let run = ws.sync();

    for name in ["empty", "file", "dangling", "linked"] {
        let e = find_entry(&run.entries, name);
        assert_eq!(e.presence, Presence::NotARepo, "{name}");
        assert_eq!(e.clone, None, "{name}");
        assert_eq!(outcomes(&run, name).clone, None, "{name}");
    }
    let dangling = find_entry(&run.entries, "dangling");
    assert_eq!(
        format!("{:?}", dangling.needs_human),
        format!(
            "[NotARepo {{ detail: \"a symlink to {}, which doesn't exist\" }}]",
            ws.outside("nowhere").display()
        )
    );
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    // untouched
    assert!(!ws.outside("nowhere").exists());
    assert_eq!(std::fs::read_dir(ws.dir("empty")).unwrap().count(), 0);
    assert_eq!(std::fs::read_to_string(ws.dir("file")).unwrap(), "mine\n");
    assert_eq!(std::fs::read_dir(ws.outside("target")).unwrap().count(), 0);
}

#[test]
fn a_path_made_before_cloning_is_held() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let path = ws.dir("app");
    // the reader's second call is the clone's own, right before it
    let calls = AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 1 {
            std::fs::create_dir(&path).unwrap();
            write(&path, "mine.txt", "mine\n");
        }
        quiet()
    };

    let run = ws.sync_with(4, &read);

    assert_eq!(
        find_entry(&run.entries, "app").clone,
        Some(verdict_act("app"))
    );
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Held {
            by: SyncHold::Changed
        }
    );
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert_eq!(files(&path), ["mine.txt"]);
    assert_eq!(root_listing(&ws), ["app", "repos.toml"]);
}

/// The recipe of an owned entry on `main`.
fn owned_recipe(key: &str) -> CloneRecipe {
    CloneRecipe {
        url: format!("git@github.com:{OWNER}/{key}"),
        branch: Some("main".into()),
        shallow: false,
        sparse: None,
    }
}

/// The `Act` verdict an owned entry on `main` gets.
fn verdict_act(key: &str) -> CloneVerdict {
    CloneVerdict::Act {
        recipe: owned_recipe(key),
    }
}

#[test]
fn a_session_at_the_missing_path_holds_its_clone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let child = LiveChild::spawn();
    // its dir was deleted from under it: the recorded cwd stays
    let deep = ws.dir("app").join("src");
    let live = LiveSessions::Known(vec![session(&child, &deep)]);

    // the preview
    let run = ws.status_live(&live);
    let held = Some(CloneVerdict::Held {
        recipe: owned_recipe("app"),
        by: HeldBy::Busy,
    });
    assert_eq!(find_entry(&run.entries, "app").clone, held);

    // as classified
    let run = ws.sync_with(4, &|| live.clone());
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Held { by: SyncHold::Busy }
    );
    assert!(!ws.dir("app").exists());

    // found right before cloning
    let calls = AtomicUsize::new(0);
    let read = || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            quiet()
        } else {
            live.clone()
        }
    };
    let run = ws.sync_with(4, &read);
    assert_eq!(
        find_entry(&run.entries, "app").clone,
        Some(verdict_act("app"))
    );
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Held { by: SyncHold::Busy }
    );
    assert!(!ws.dir("app").exists());

    // recorded through a symlink to the root: resolved, it's there
    let link = ws.outside("link");
    std::os::unix::fs::symlink(ws.root(), &link).unwrap();
    let linked = LiveSessions::Known(vec![session(&child, &link.join("app"))]);
    let run = ws.sync_with(4, &|| linked.clone());
    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Held { by: SyncHold::Busy }
    );
    assert!(!ws.dir("app").exists());

    // a session beside it, at a path sharing its name's prefix, holds nothing
    let beside = LiveSessions::Known(vec![session(&child, &ws.dir("app-wt"))]);
    let run = ws.sync_with(4, &|| beside.clone());
    assert!(matches!(cloned(&run, "app"), CloneOutcome::Cloned { .. }));
    assert!(ws.ssh_log().len() == 1, "{:?}", ws.ssh_log());
}

/// A missing dir holds no work, and the clone never replaces anything:
/// busy detection that can't vouch for every session holds no clone.
#[test]
fn unavailable_busy_detection_still_clones() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let run = ws.sync_with(4, &|| LiveSessions::Unavailable(Unavailable::HomeUnknown));
    assert!(matches!(cloned(&run, "app"), CloneOutcome::Cloned { .. }));
    ws.assert_head(&ws.dir("app"), Some("main"));
}

#[test]
fn an_agent_clones() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let run = ws.sync_as(Caller::Agent, 4, &quiet);
    assert!(matches!(cloned(&run, "app"), CloneOutcome::Cloned { .. }));
    ws.assert_head(&ws.dir("app"), Some("main"));
}

/// Another entry's worktree, since deleted, was at the missing path: git
/// still records it there, and would take the clone for its files.
#[test]
fn another_entrys_gone_worktree_at_the_path_holds_its_clone() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.remote("lib", &[]);
    ws.declare_repo("lib", "lib", "");
    ws.write_registry();
    ws.add_worktree(&app, &ws.dir("lib"), &["-b", "wt"]);
    std::fs::remove_dir_all(ws.dir("lib")).unwrap();
    assert!(
        ws.git(&app, &["worktree", "list", "--porcelain"])
            .contains("prunable")
    );

    let run = ws.sync();

    let recipe = owned_recipe("lib");
    assert_eq!(
        find_entry(&run.entries, "lib").clone,
        Some(CloneVerdict::Held {
            recipe,
            by: HeldBy::UnprobedWorktree
        })
    );
    assert_eq!(
        cloned(&run, "lib"),
        CloneOutcome::Held {
            by: SyncHold::UnprobedWorktree
        }
    );
    assert!(!ws.dir("lib").exists());
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

/// The clone may use its entry's transport alone: an `insteadOf` pointing
/// it anywhere else — here at the very bare remote, over `file` — fails.
#[test]
fn a_clone_uses_its_entrys_transport_alone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    ws.serve_https();
    ws.write_registry();
    let file_url = |name: &str| format!("file://{}", ws.bare(name).display());
    let (app_url, lib_url) = (file_url("app"), file_url("lib"));
    ws.set_env("GIT_CONFIG_COUNT", "2");
    ws.set_env("GIT_CONFIG_KEY_0", format!("url.{app_url}.insteadOf"));
    ws.set_env("GIT_CONFIG_VALUE_0", format!("git@github.com:{OWNER}/app"));
    ws.set_env("GIT_CONFIG_KEY_1", format!("url.{lib_url}.insteadOf"));
    ws.set_env(
        "GIT_CONFIG_VALUE_1",
        format!("https://github.com/{THIRD_PARTY}/lib"),
    );
    // the fixture's own allowlist takes `file`: the rewrite would reach
    let probe = ws.git_output(
        ws.base(),
        &["ls-remote", &format!("git@github.com:{OWNER}/app"), "HEAD"],
    );
    assert!(probe.status.success());

    let run = ws.sync();

    for key in ["app", "lib"] {
        assert_eq!(
            cloned(&run, key),
            CloneOutcome::CloneFailed {
                failure: RemoteFailure::Failed {
                    message: "fatal: transport 'file' not allowed".into()
                }
            },
            "{key}"
        );
    }
    assert_eq!(root_listing(&ws), ["repos.toml"]);
}

#[test]
fn a_host_that_refuses_fails_the_clone() {
    let mut ws = FixtureWorkspace::new();
    // the fixture's `ssh` serves github.com alone: any other host is refused
    ws.declare_repo_url("elsewhere", "https://example.com/me/elsewhere", "public");
    ws.write_registry();
    let run = ws.sync();
    assert_eq!(
        cloned(&run, "elsewhere"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::Failed {
                message: "fatal: Could not read from remote repository.".into()
            }
        }
    );
    assert!(
        ws.ssh_log()
            .iter()
            .all(|l| l.contains("git@example.com git-upload-pack 'me/elsewhere'")),
        "{:?}",
        ws.ssh_log()
    );
    assert_eq!(root_listing(&ws), ["repos.toml"]);
}

#[test]
fn clone_outcomes_are_the_same_whatever_the_jobs() {
    let build = || {
        let mut ws = FixtureWorkspace::new();
        for name in ["a", "b", "c", "d"] {
            ws.remote(name, &[]);
            ws.upstream_commit(name, "main");
            ws.declare_repo(name, name, "");
        }
        ws.declare_repo("gone", "gone", "");
        let _ = ws.owned_repo("present", &[]);
        ws.upstream_commit("present", "main");
        ws.write_registry();
        ws
    };
    let (one, many) = (build(), build());
    let serial = one.sync_with(1, &quiet);
    let parallel = many.sync_with(16, &quiet);
    assert_eq!(serial.outcomes, parallel.outcomes);
    for name in ["a", "b", "c", "d"] {
        assert!(
            matches!(cloned(&serial, name), CloneOutcome::Cloned { .. }),
            "{name}"
        );
    }
    assert!(matches!(
        cloned(&serial, "gone"),
        CloneOutcome::CloneFailed { .. }
    ));
    assert!(outcomes(&serial, "present").clone.is_none());
    assert_eq!(
        root_listing(&one),
        ["a", "b", "c", "d", "present", "repos.toml"]
    );
    assert_eq!(root_listing(&many), root_listing(&one));
}

/// SSH runs in batch mode, as the fetch's does — unless the user's config
/// names its own SSH command, which batch mode's `GIT_SSH_COMMAND` would
/// override.
#[test]
fn an_owned_clone_runs_ssh_in_batch_mode_unless_the_user_configures_ssh() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let run = ws.sync();
    assert!(matches!(cloned(&run, "app"), CloneOutcome::Cloned { .. }));
    assert_eq!(
        ws.ssh_log(),
        [format!(
            "-o BatchMode=yes -o ConnectTimeout=15 -o SendEnv=GIT_PROTOCOL git@github.com \
             git-upload-pack '{OWNER}/app'"
        )]
    );

    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    ws.set_env("GIT_CONFIG_COUNT", "1");
    ws.set_env("GIT_CONFIG_KEY_0", "core.sshCommand");
    ws.set_env("GIT_CONFIG_VALUE_0", "ssh -o Mine=yes");
    let run = ws.sync();
    assert!(matches!(cloned(&run, "app"), CloneOutcome::Cloned { .. }));
    assert_eq!(
        ws.ssh_log(),
        [format!(
            "-o Mine=yes -o SendEnv=GIT_PROTOCOL git@github.com git-upload-pack '{OWNER}/app'"
        )]
    );
}

/// Without `serve_https`, the fixture's `git-remote-https` refuses every
/// URL: a clone allowing `https` alone never reaches a real host.
#[test]
fn a_third_party_clone_without_the_fixtures_https_reaches_no_host() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    ws.write_registry();

    let run = ws.sync();

    let url = format!("https://github.com/{THIRD_PARTY}/lib");
    // the helper exits before speaking the protocol
    assert_eq!(
        cloned(&run, "lib"),
        CloneOutcome::CloneFailed {
            failure: RemoteFailure::Failed {
                message: "fatal: remote helper 'https' aborted session".into()
            }
        }
    );
    assert_eq!(ws.https_refused_log(), [url]);
    assert!(ws.https_log().is_empty());
    assert_eq!(root_listing(&ws), ["repos.toml"]);
}

/// A missing entry naming the same repo as another — here a registered
/// worktree dir whose record was pruned — is never cloned: a second copy
/// would be a guess.
#[test]
fn a_missing_entry_sharing_its_repo_is_held_for_a_person() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.declare_repo("app_wt", "app", "dir = \"app-wt\"");
    ws.write_registry();
    ws.add_worktree(&app, &ws.dir("app-wt"), &["-b", "wt"]);
    std::fs::remove_dir_all(ws.dir("app-wt")).unwrap();
    ws.git(&app, &["worktree", "prune"]);
    assert!(
        !ws.git(&app, &["worktree", "list", "--porcelain"])
            .contains("app-wt")
    );

    let run = ws.sync();

    let held = Some(CloneVerdict::Held {
        recipe: owned_recipe("app"),
        by: HeldBy::Entry,
    });
    let e = find_entry(&run.entries, "app_wt");
    assert_eq!(e.presence, Presence::Missing);
    assert_eq!(e.clone, held);
    assert_eq!(
        e.needs_human,
        [NeedsHuman::CloneSharesRepo { with: "app".into() }]
    );
    assert_eq!(
        cloned(&run, "app_wt"),
        CloneOutcome::Held {
            by: SyncHold::Entry
        }
    );
    // the present one is untouched by it
    let e = find_entry(&run.entries, "app");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    assert!(!ws.dir("app-wt").exists());
    // no clone was tried (app's fetch reaches its bare remote over `file`)
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert_eq!(root_listing(&ws), ["app", "repos.toml"]);
}

/// A remote whose HEAD is detached at a commit no branch is on clones
/// detached: an entry with no branch reads back failed, the clone left in
/// place.
#[test]
fn a_clone_on_a_detached_head_fails_its_read_back_and_stays() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[]);
    let first = ws.git(&ws.bare("lib"), &["rev-parse", "main"]);
    ws.upstream_commit("lib", "main");
    ws.git(
        &ws.bare("lib"),
        &["update-ref", "--no-deref", "HEAD", &first],
    );
    ws.git_fails(&ws.bare("lib"), &["symbolic-ref", "-q", "HEAD"]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    ws.serve_https();
    ws.write_registry();
    let lib = ws.dir("lib");

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "lib"),
        CloneOutcome::Failed {
            message: format!("cloned into {}, but its HEAD is detached", lib.display())
        }
    );
    ws.assert_head(&lib, None);
    assert_eq!(ws.git(&lib, &["rev-parse", "HEAD"]), first);
    assert_eq!(root_listing(&ws), ["lib", "repos.toml"]);
}

/// The user's config can leave the cloned branch tracking something other
/// than origin's branch of the name: read back failed, left in place.
#[test]
fn a_clone_whose_upstream_isnt_origins_fails_its_read_back_and_stays() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    ws.set_env("GIT_CONFIG_COUNT", "1");
    ws.set_env("GIT_CONFIG_KEY_0", "branch.main.remote");
    ws.set_env("GIT_CONFIG_VALUE_0", ".");
    let app = ws.dir("app");

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Failed {
            message: format!(
                "cloned into {}, but main's upstream is refs/heads/main, not \
                 refs/remotes/origin/main",
                app.display()
            )
        }
    );
    ws.assert_head(&app, Some("main"));
    assert_eq!(root_listing(&ws), ["app", "repos.toml"]);
}

/// A filter driver the user's config defines runs in the clone's checkout,
/// as in any checkout — here one that leaves a file behind, so the
/// checkout reads dirty: read back failed, left in place.
#[test]
fn a_clone_that_reads_dirty_fails_its_read_back_and_stays() {
    let mut ws = FixtureWorkspace::new();
    ws.remote(
        "app",
        &[(".gitattributes", "*.txt filter=mark\n"), ("a.txt", "a\n")],
    );
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    ws.set_env("GIT_CONFIG_COUNT", "1");
    ws.set_env("GIT_CONFIG_KEY_0", "filter.mark.smudge");
    ws.set_env("GIT_CONFIG_VALUE_0", "touch smudged; cat");
    let app = ws.dir("app");

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Failed {
            message: format!(
                "cloned into {}, but its checkout has 1 uncommitted changes",
                app.display()
            )
        }
    );
    ws.assert_porcelain(&app, &["?? smudged"]);
    assert_eq!(root_listing(&ws), ["app", "repos.toml"]);
}

/// A clone's temp dir left behind by a run that was killed is the tool's
/// own leftover, `.git` or not — and one this process would make is
/// never reused, the failure naming it. Only the exact name counts.
#[test]
fn a_leftover_temp_dir_is_named_and_never_reused() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.write_registry();
    let killed = ws.dir(".app.repos-clone-1-0123456789abcdef");
    ws.git(
        &ws.root(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            &format!("file://{}", ws.bare("app").display()),
            killed.to_str().unwrap(),
        ],
    );
    ws.git(
        &killed,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("git@github.com:{OWNER}/app"),
        ],
    );
    let ours = ws.dir(&temp_dir_name("app"));
    std::fs::create_dir(&ours).unwrap();
    // not the tool's: no nonce, the shape before it had one
    let other = ws.dir(".app.repos-clone-2");
    ws.git(&ws.root(), &["init", "-q", other.to_str().unwrap()]);

    let run = ws.sync();

    assert_eq!(
        cloned(&run, "app"),
        CloneOutcome::Failed {
            message: format!(
                "the temp dir {} is already there, a clone repos didn't finish, or one \
                 still running: remove it once no `repos sync` is running, then rerun",
                ours.display()
            )
        }
    );
    assert!(!ws.dir("app").exists());
    assert_eq!(std::fs::read_dir(&ours).unwrap().count(), 0);
    let found: Vec<(String, UnregisteredKind, bool)> = ws
        .unregistered()
        .into_iter()
        .map(|u| (u.dir, u.kind, u.owned))
        .collect();
    let mut want = vec![
        (
            ".app.repos-clone-1-0123456789abcdef".to_owned(),
            UnregisteredKind::UnfinishedClone,
            true,
        ),
        (
            ".app.repos-clone-2".to_owned(),
            UnregisteredKind::Clone,
            false,
        ),
        (
            ours.file_name().unwrap().to_str().unwrap().to_owned(),
            UnregisteredKind::UnfinishedClone,
            false,
        ),
    ];
    // by name
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(found, want);
}
