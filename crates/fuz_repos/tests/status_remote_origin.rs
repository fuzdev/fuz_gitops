//! What origin advice can reach: config scopes, URL lists, and the repo config.

mod support;

use std::ffi::OsString;
use std::path::Path;

use fuz_repos::classify::{OriginByHand, OriginFix, OriginRemote};
use fuz_repos::git::Git;
use fuz_repos::remote::{RefGoneFix, RemoteFailure};
use fuz_repos::state::{Presence, ProbeErrorKind};
use support::remote::drift;
use support::{FixtureWorkspace, find_entry};

/// A runner whose git reads `global` as its global config.
fn runner_with_global(ws: &FixtureWorkspace, global: &Path) -> Git {
    let mut env: Vec<(OsString, OsString)> = ws
        .env()
        .into_iter()
        .filter(|(k, _)| k != "GIT_CONFIG_GLOBAL")
        .collect();
    env.push(("GIT_CONFIG_GLOBAL".into(), global.into()));
    Git::with_clean_env(env)
}

#[test]
fn origin_urls_are_read_as_git_reads_them() {
    let mut ws = FixtureWorkspace::new();
    for name in ["multi", "first", "reset", "empty", "valueless"] {
        ws.owned_repo(name, &[]);
    }
    // a single empty value, and one with no value at all (written by hand:
    // `git config` can't write it)
    let empty = ws.dir("empty");
    ws.git(&empty, &["config", "remote.origin.url", ""]);
    let valueless = ws.dir("valueless");
    ws.git(&valueless, &["config", "--unset", "remote.origin.url"]);
    let config = valueless.join(".git/config");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{text}[remote \"origin\"]\n\turl\n")).unwrap();
    let out = ws.git_output(&valueless, &["remote", "get-url", "origin"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing value for 'remote.origin.url'"));
    // several URLs, a stale one first: git fetches from it, `set-url` fails
    let multi = ws.dir("multi");
    let old = "https://me:ghp_TOKEN@github.com/old/multi";
    ws.git(&multi, &["config", "--unset-all", "remote.origin.url"]);
    ws.git(&multi, &["config", "--add", "remote.origin.url", old]);
    ws.git(
        &multi,
        &[
            "config",
            "--add",
            "remote.origin.url",
            &support::owned_origin("multi"),
        ],
    );
    assert_eq!(ws.git(&multi, &["remote", "get-url", "origin"]), old);
    let out = ws.git_output(&multi, &["remote", "set-url", "origin", "x"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).contains("remote.origin.url has multiple values"));
    // the registry's URL first, a mirror after: git fetches from the first
    let first = ws.dir("first");
    ws.git(
        &first,
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://mirror.example/me/first",
        ],
    );
    let listed = ws.git(&first, &["config", "--get-all", "remote.origin.url"]);
    assert_eq!(
        listed.lines().next(),
        Some(support::owned_origin("first").as_str())
    );
    assert_eq!(listed.lines().count(), 2);
    // an empty value resets the list: no URL at all
    let reset = ws.dir("reset");
    ws.git(&reset, &["config", "--add", "remote.origin.url", ""]);
    assert_eq!(ws.git(&reset, &["remote", "get-url", "origin"]), "origin");

    let entries = ws.status_with_fetch();
    // several URLs, the first a mismatch: fixed by hand
    assert_eq!(
        drift(find_entry(&entries, "multi")),
        Some((
            OriginRemote::Url {
                url: "https://***@github.com/old/multi".into()
            },
            OriginFix::ByHand {
                reason: OriginByHand::SeveralUrls
            }
        ))
    );
    // several URLs, the registry's first: no drift
    assert_eq!(drift(find_entry(&entries, "first")), None);
    let e = find_entry(&entries, "reset");
    assert_eq!(
        drift(e),
        Some((
            OriginRemote::NoUrl,
            OriginFix::ByHand {
                reason: OriginByHand::EmptyValue
            }
        ))
    );
    // nothing to fetch from: no fetch ran
    assert_eq!(e.fetch_error, None);
    assert!(!reset.join(".git/FETCH_HEAD").exists());
    // a single empty value: `set-url` replaces it
    let e = find_entry(&entries, "empty");
    assert_eq!(drift(e), Some((OriginRemote::NoUrl, OriginFix::SetUrl)));
    assert!(!empty.join(".git/FETCH_HEAD").exists());
    // a valueless one breaks git's remote code: with a tracking branch,
    // `git status --branch` itself fails, and the probe fails closed with
    // git's message (the advice for a repo that probes is `ValuelessUrl`,
    // pinned in classify's tests); no fetch was tried
    let e = find_entry(&entries, "valueless");
    let error = e.probe_error.as_ref().unwrap();
    assert_eq!(error.kind, ProbeErrorKind::GitFailed, "{e:?}");
    assert!(
        error
            .message
            .contains("missing value for 'remote.origin.url'"),
        "{e:?}"
    );
    assert!(!valueless.join(".git/FETCH_HEAD").exists());
    // the advice holds: `set-url` fixes the empty one, fails the valueless
    ws.git(
        &empty,
        &[
            "remote",
            "set-url",
            "origin",
            &support::owned_origin("empty"),
        ],
    );
    assert_eq!(drift(&support::take_entry(ws.status(), "empty")), None);
    assert!(
        !ws.git_output(&valueless, &["remote", "set-url", "origin", "x"])
            .status
            .success()
    );
    // no credential anywhere in the report
    let json =
        serde_json::to_string(&entries.iter().map(|e| &e.needs_human).collect::<Vec<_>>()).unwrap();
    assert!(!json.contains("ghp_TOKEN"), "{json}");
}

#[test]
fn origin_advice_keys_on_what_the_repo_config_holds() {
    let mut ws = FixtureWorkspace::new();
    for name in ["global_url", "global_fetch", "included"] {
        ws.owned_repo(name, &[]);
        ws.git(&ws.dir(name), &["remote", "remove", "origin"]);
    }
    // a global config naming an origin for every repo: a stale URL there
    // for one, only a refspec for the others
    let global = ws.outside("global.gitconfig");
    support::write(
        ws.base(),
        "global.gitconfig",
        "[remote \"origin\"]\n\tfetch = +refs/pull/*/head:refs/remotes/origin/pr/*\n",
    );
    let git = runner_with_global(&ws, &global);
    let gurl = ws.dir("global_url");
    // the included case: the repo's config includes a file with the URL
    let inc = ws.outside("inc.gitconfig");
    support::write(
        ws.base(),
        "inc.gitconfig",
        "[remote \"origin\"]\n\turl = git@github.com:old/included\n",
    );
    let included = ws.dir("included");
    ws.git(
        &included,
        &["config", "include.path", inc.to_str().unwrap()],
    );
    // the stale global URL: only `global_url` reads it
    let gurl_global = ws.outside("gurl.gitconfig");
    support::write(
        ws.base(),
        "gurl.gitconfig",
        "[remote \"origin\"]\n\turl = git@github.com:old/global_url\n",
    );

    let entries = ws.status_with(&ws.root(), false, &git, &ws.visibility_base());
    // known to git only through a global refspec: `remote add` works
    let (origin, fix) = drift(find_entry(&entries, "global_fetch")).unwrap();
    assert_eq!((origin, fix), (OriginRemote::NoUrl, OriginFix::Add));
    // a URL from an included file: `set-url` would add one after it
    let (origin, fix) = drift(find_entry(&entries, "included")).unwrap();
    assert_eq!(
        (origin, fix),
        (
            OriginRemote::Url {
                url: "git@github.com:old/included".into()
            },
            OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile
            }
        )
    );
    // a URL only in global config: `set-url` says `No such remote`, and an
    // added one would come second
    let git = runner_with_global(&ws, &gurl_global);
    let e = support::take_entry(
        ws.status_with(&ws.root(), false, &git, &ws.visibility_base()),
        "global_url",
    );
    assert_eq!(
        drift(&e),
        Some((
            OriginRemote::Url {
                url: "git@github.com:old/global_url".into()
            },
            OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile
            }
        ))
    );
    let with_global = |repo: &Path, args: &[&str]| {
        let mut cmd = ws.command("git", repo);
        cmd.env("GIT_CONFIG_GLOBAL", &gurl_global).args(args);
        cmd.output().unwrap()
    };
    let out = with_global(&gurl, &["remote", "set-url", "origin", "x"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("No such remote 'origin'"));
    assert!(
        with_global(&gurl, &["remote", "add", "origin", "x"])
            .status
            .success()
    );
    let out = with_global(&gurl, &["remote", "get-url", "origin"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "git@github.com:old/global_url"
    );

    // and the advised `remote add` does work where it's advised
    let mut cmd = ws.command("git", &ws.dir("global_fetch"));
    cmd.env("GIT_CONFIG_GLOBAL", &global).args([
        "remote",
        "add",
        "origin",
        &support::owned_origin("global_fetch"),
    ]);
    assert!(cmd.output().unwrap().status.success());
}

#[test]
fn a_refspec_the_repo_config_cannot_drop_is_not_advised_away() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("spec", &[]);
    ws.remote("neg", &[]);
    for name in ["spec", "neg"] {
        ws.upstream_commit(name, "solo");
        ws.declare_repo(name, name, "");
        ws.clone_owned(name, name, &["--single-branch", "--branch", "solo"]);
        ws.git(&ws.dir(name), &["checkout", "-q", "-b", "main"]);
    }
    // a global refspec beside `spec`'s one, a negative one beside `neg`'s
    let global = ws.outside("global.gitconfig");
    support::write(
        ws.base(),
        "global.gitconfig",
        "[remote \"origin\"]\n\tfetch = +refs/pull/*/head:refs/remotes/origin/pr/*\n",
    );
    let neg = ws.dir("neg");
    ws.git(
        &neg,
        &["config", "--add", "remote.origin.fetch", "^refs/heads/wip"],
    );
    for name in ["spec", "neg"] {
        ws.upstream_delete_branch(name, "solo");
    }
    let git = runner_with_global(&ws, &global);

    let entries = ws.status_with(&ws.root(), true, &git, &ws.visibility_base());
    for name in ["spec", "neg"] {
        // dropping the line would leave only a global or negative refspec,
        // and a fetch that updates no branch: repoint at the registry's
        assert_eq!(
            find_entry(&entries, name).fetch_error,
            Some(RemoteFailure::RefGone {
                refname: "refs/heads/solo".into(),
                fix: RefGoneFix::SetBranches {
                    branch: Some("main".into())
                },
            }),
            "{name}"
        );
    }
    // a gone ref named only by a global refspec is out of reach
    support::write(
        ws.base(),
        "global.gitconfig",
        "[remote \"origin\"]\n\tfetch = +refs/heads/gone:refs/remotes/origin/gone\n",
    );
    let spec = ws.dir("spec");
    ws.git(&spec, &["remote", "set-branches", "origin", "main"]);
    let e = support::take_entry(
        ws.status_with(&ws.root(), true, &git, &ws.visibility_base()),
        "spec",
    );
    assert_eq!(
        e.fetch_error,
        Some(RemoteFailure::RefGone {
            refname: "refs/heads/gone".into(),
            fix: RefGoneFix::ByHand,
        })
    );
}

#[test]
fn the_repo_config_file_is_found_from_a_linked_worktree_too() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.git(
        &app,
        &["remote", "set-url", "origin", "git@github.com:old/app"],
    );
    // a second entry whose dir is a linked worktree of the same repo: git
    // names the repo's config file by an absolute path there, relative from
    // the main checkout
    ws.declare_repo("feat", "app", "dir = \"app-feat\"\nbranch = \"feat\"");
    ws.add_worktree(&app, &ws.dir("app-feat"), &["-b", "feat"]);
    let origins = ws.git(
        &ws.dir("app-feat"),
        &["config", "--show-origin", "--get-all", "remote.origin.url"],
    );
    assert!(
        origins.starts_with(&format!("file:{}", ws.base().display())),
        "{origins}"
    );
    assert!(
        ws.git(
            &app,
            &["config", "--show-origin", "--get-all", "remote.origin.url"]
        )
        .starts_with("file:.git/config")
    );

    let entries = ws.status();
    for key in ["app", "feat"] {
        assert_eq!(
            drift(find_entry(&entries, key)).map(|(_, fix)| fix),
            Some(OriginFix::SetUrl),
            "{key}"
        );
    }
}

#[test]
fn a_config_file_whose_path_is_not_utf8_still_probes() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    ws.owned_repo("old", &[]);
    ws.git(&ws.dir("old"), &["remote", "remove", "origin"]);
    // a global config at a path git prints raw in `--show-origin`
    let global = ws
        .base()
        .join(std::ffi::OsStr::from_bytes(b"g\xff.gitconfig"));
    std::fs::write(
        &global,
        "[branch \"main\"]\n\tremote = origin\n[remote \"origin\"]\n\turl = git@github.com:old/old\n",
    )
    .unwrap();
    let git = runner_with_global(&ws, &global);
    // control: git prints the path's raw byte
    let mut cmd = ws.command("git", &ws.dir("app"));
    cmd.env("GIT_CONFIG_GLOBAL", &global).args([
        "config",
        "-z",
        "--show-origin",
        "--get-all",
        "branch.main.remote",
    ]);
    let out = cmd.output().unwrap().stdout;
    assert!(out.windows(2).any(|w| w == b"g\xff"), "{out:?}");

    let entries = ws.status_with(&ws.root(), false, &git, &ws.visibility_base());
    let app = find_entry(&entries, "app");
    assert_eq!(app.probe_error, None);
    assert_eq!(app.presence, Presence::Present);
    // `old` has only the global's URL: never taken for the repo's own
    let old = find_entry(&entries, "old");
    assert_eq!(old.probe_error, None);
    assert_eq!(
        drift(old),
        Some((
            OriginRemote::Url {
                url: "git@github.com:old/old".into()
            },
            OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile
            }
        ))
    );
}
