//! Fetch failures classified from the stderr of a real git, SSH ones through a
//! fake `ssh` that prints ssh's real failure lines, and what counts as fetched.
//! Nothing leaves the machine.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use fuz_repos::classify::{NeedsHuman, OriginFix, OriginRemote};
use fuz_repos::git::Git;
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause};
use fuz_repos::state::{Presence, Relation};
use support::{FixtureWorkspace, OWNER, branch, find_entry};

/// `FETCH_HEAD`'s length and mtime in unix seconds.
fn fetch_head(repo: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(repo.join(".git/FETCH_HEAD")).unwrap();
    let mtime = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap();
    (meta.len(), mtime.as_secs())
}

/// Sets `FETCH_HEAD`'s mtime to the fixture clock's start.
fn backdate_fetch_head(repo: &Path) {
    support::set_mtime(
        &repo.join(".git/FETCH_HEAD"),
        UNIX_EPOCH + Duration::from_secs(support::CLOCK_START),
    );
}

#[test]
fn a_deleted_branch_under_a_narrowed_refspec_is_ref_gone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("spec", &[]);
    for b in ["feat", "fork"] {
        ws.upstream_commit("spec", b);
    }
    ws.declare_repo("spec", "spec", "");
    let spec = ws.clone_owned("spec", "spec", &["--single-branch", "--branch", "main"]);
    for b in ["feat", "fork"] {
        ws.git(&spec, &["remote", "set-branches", "--add", "origin", b]);
    }
    ws.git(&spec, &["fetch", "-q", "origin"]);
    let refspecs =
        |ws: &FixtureWorkspace| ws.git(&spec, &["config", "--get-all", "remote.origin.fetch"]);
    assert_eq!(
        refspecs(&ws),
        "+refs/heads/main:refs/remotes/origin/main\n\
         +refs/heads/feat:refs/remotes/origin/feat\n\
         +refs/heads/fork:refs/remotes/origin/fork"
    );
    // an earlier fetch, a while ago
    backdate_fetch_head(&spec);
    assert!(fetch_head(&spec).0 > 0);
    let e = support::take_entry(ws.status(), "spec");
    assert_eq!(e.fetched_at, Some(support::CLOCK_START));
    ws.upstream_delete_branch("spec", "feat");

    let e = support::take_entry(ws.status_with_fetch(), "spec");
    let pattern = r"^\+?refs/heads/feat(:|$)";
    assert_eq!(
        e.fetch_error,
        Some(RemoteFailure::RefGone {
            refname: "refs/heads/feat".into(),
            fix: RefGoneFix::UnsetRefspec {
                pattern: pattern.into()
            },
        })
    );
    // nothing fetched, nothing pruned: the local view stands
    assert!(ws.has_ref(&spec, "refs/remotes/origin/feat"));
    assert_eq!(e.probe_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
    // git emptied FETCH_HEAD, freshening its mtime over refs no fetch
    // updated: the remote view's age is unknown, not "just now"
    let (len, mtime) = fetch_head(&spec);
    assert_eq!(len, 0);
    assert!(mtime > support::CLOCK_START);
    assert_eq!(e.fetched_at, None);

    // the advised repair drops that refspec alone, and the next fetch works
    ws.git(
        &spec,
        &["config", "--unset-all", "remote.origin.fetch", pattern],
    );
    assert_eq!(
        refspecs(&ws),
        "+refs/heads/main:refs/remotes/origin/main\n\
         +refs/heads/fork:refs/remotes/origin/fork"
    );
    let e = support::take_entry(ws.status_with_fetch(), "spec");
    assert_eq!(e.fetch_error, None);
    assert!(e.fetched_at.is_some());
}

#[test]
fn a_deleted_branch_that_is_the_only_refspec_is_repointed() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("spec", &[]);
    ws.upstream_commit("spec", "solo");
    ws.declare_repo("spec", "spec", "branch = \"solo\"");
    let spec = ws.clone_owned("spec", "spec", &["--single-branch", "--branch", "solo"]);
    assert_eq!(
        ws.git(&spec, &["config", "--get-all", "remote.origin.fetch"]),
        "+refs/heads/solo:refs/remotes/origin/solo"
    );
    ws.upstream_delete_branch("spec", "solo");

    let e = support::take_entry(ws.status_with_fetch(), "spec");
    // dropping the only refspec would leave a fetch that updates nothing
    assert_eq!(
        e.fetch_error,
        Some(RemoteFailure::RefGone {
            refname: "refs/heads/solo".into(),
            // the registry follows `solo`, the branch that's gone: no
            // branch to name
            fix: RefGoneFix::SetBranches { branch: None },
        })
    );
    ws.git(&spec, &["remote", "set-branches", "origin", "main"]);
    let e = support::take_entry(ws.status_with_fetch(), "spec");
    assert_eq!(e.fetch_error, None);
    assert!(ws.has_ref(&spec, "refs/remotes/origin/main"));
}

#[test]
fn only_a_fetch_that_wrote_refs_counts_as_fetched() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.remote("shallow", &[]);
    ws.declare_repo("shallow", "shallow", "");
    let shallow = ws.clone_owned("shallow", "shallow", &["--depth", "1"]);
    ws.assert_shallow(&shallow, true);
    // an empty remote: a fetch succeeds, writing nothing
    let empty = ws.bare("empty");
    ws.git(
        ws.base(),
        &["init", "-q", "--bare", empty.to_str().unwrap()],
    );
    ws.declare_repo("empty", "empty", "");
    let empty_clone = ws.dir("empty");
    ws.git(&ws.root(), &["init", "-q", "empty"]);
    ws.git(
        &empty_clone,
        &["remote", "add", "origin", &support::owned_origin("empty")],
    );
    ws.set_origin(&empty_clone, "empty", &support::owned_origin("empty"));

    let entries = ws.status_with_fetch();
    for (key, repo) in [("app", &app), ("shallow", &shallow)] {
        let e = find_entry(&entries, key);
        assert_eq!(e.fetch_error, None, "{key}");
        // a fetch with nothing new still writes a line per ref
        assert!(fetch_head(repo).0 > 0, "{key}");
        assert!(e.fetched_at.is_some(), "{key}");
    }
    ws.assert_shallow(&shallow, true);
    let e = find_entry(&entries, "empty");
    assert_eq!(e.fetch_error, None);
    assert_eq!(fetch_head(&empty_clone).0, 0);
    assert_eq!(e.fetched_at, None);

    // a second run, nothing new upstream: still fetched
    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert!(fetch_head(&app).0 > 0);
    assert!(e.fetched_at.is_some());
}

#[test]
fn a_fresh_clone_is_dated_by_its_clone_entry() {
    let mut ws = FixtureWorkspace::new();
    for name in ["plain", "shallow", "single"] {
        ws.remote(name, &[]);
    }
    ws.upstream_commit("single", "dev");
    ws.declare_repo("plain", "plain", "");
    ws.declare_repo("shallow", "shallow", "");
    ws.declare_repo("single", "single", "branch = \"dev\"");
    let plain = ws.clone_owned("plain", "plain", &[]);
    let shallow = ws.clone_owned("shallow", "shallow", &["--depth", "1", "--no-tags"]);
    let single = ws.clone_owned("single", "single", &["--single-branch", "--branch", "dev"]);
    ws.assert_shallow(&shallow, true);
    // a single-branch clone writes no `origin/HEAD` or its reflog; every
    // clone writes `HEAD`'s
    assert!(!ws.has_ref(&single, "refs/remotes/origin/HEAD"));
    let entries = ws.status();
    for (key, repo) in [
        ("plain", &plain),
        ("shallow", &shallow),
        ("single", &single),
    ] {
        assert!(!repo.join(".git/FETCH_HEAD").exists(), "{key}");
        let cloned_at = ws.clone_reflog_time(repo);
        // the entry's ident date, the fixture's clock, not a file's mtime
        assert!(
            (support::CLOCK_START..support::CLOCK_START + 86_400).contains(&cloned_at),
            "{key}: {cloned_at}"
        );
        assert_eq!(
            find_entry(&entries, key).fetched_at,
            Some(cloned_at),
            "{key}"
        );
    }
    // moving HEAD appends to its reflog: the clone's stays first
    ws.git(&plain, &["checkout", "-q", "-b", "feat"]);
    assert_eq!(
        ws.entry("plain").fetched_at,
        Some(ws.clone_reflog_time(&plain))
    );
}

#[test]
fn a_failed_fetch_after_a_clone_is_never_fetched() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    let lib = ws.owned_repo("lib", &[]);
    let wt = ws.dir("lib-feat");
    let admin = ws.add_worktree(&lib, &wt, &["-b", "feat"]);
    // a failed fetch empties `FETCH_HEAD`: the remote view's age is
    // unknown, so the clone's time doesn't stand in
    for repo in [&app, &wt] {
        let out = ws.git_output(repo, &["fetch", "-q", "origin", "refs/heads/nope"]);
        assert!(!out.status.success());
    }
    assert_eq!(fetch_head(&app).0, 0);
    // in any worktree's git dir alone
    assert!(!lib.join(".git/FETCH_HEAD").exists());
    assert_eq!(
        std::fs::metadata(admin.join("FETCH_HEAD")).unwrap().len(),
        0
    );
    let entries = ws.status();
    for key in ["app", "lib"] {
        assert_eq!(find_entry(&entries, key).fetched_at, None, "{key}");
    }
}

#[test]
fn a_repo_with_no_clone_entry_is_never_fetched() {
    let mut ws = FixtureWorkspace::new();
    // made by `git init`: no clone, no reflog yet
    ws.remote("made", &[]);
    ws.declare_repo("made", "made", "");
    let made = ws.dir("made");
    ws.git(&ws.root(), &["init", "-q", "made"]);
    ws.git(
        &made,
        &["remote", "add", "origin", &support::owned_origin("made")],
    );
    ws.set_origin(&made, "made", &support::owned_origin("made"));
    assert!(!made.join(".git/logs/HEAD").exists());
    // cloned, but its reflog expired and HEAD moved since: the first entry
    // is a checkout's
    let old = ws.owned_repo("old", &[]);
    ws.git(&old, &["reflog", "expire", "--expire=now", "--all"]);
    ws.git(&old, &["checkout", "-q", "-b", "feat"]);
    let first = std::fs::read_to_string(old.join(".git/logs/HEAD")).unwrap();
    assert!(
        first.lines().count() == 1 && first.contains("\tcheckout: "),
        "{first}"
    );
    let entries = ws.status();
    for (key, repo) in [("made", &made), ("old", &old)] {
        assert!(!repo.join(".git/FETCH_HEAD").exists(), "{key}");
        assert_eq!(find_entry(&entries, key).fetched_at, None, "{key}");
    }
}

#[test]
fn an_entry_without_an_origin_url_is_not_fetched() {
    let mut ws = FixtureWorkspace::new();
    for name in ["gone", "bare"] {
        ws.owned_repo(name, &[]);
    }
    // no origin remote at all, and one with keys but no URL
    let gone = ws.dir("gone");
    ws.git(&gone, &["remote", "remove", "origin"]);
    assert_eq!(ws.git(&gone, &["remote"]), "");
    let bare = ws.dir("bare");
    ws.git(&bare, &["config", "--unset", "remote.origin.url"]);
    assert!(
        ws.git_output(&bare, &["config", "remote.origin.url"])
            .status
            .code()
            == Some(1)
    );
    assert!(
        !ws.git(&bare, &["config", "--get-regexp", "^remote\\.origin\\."])
            .is_empty()
    );

    let entries = ws.status_with_fetch();
    for (key, repo, origin, fix) in [
        ("gone", &gone, OriginRemote::Missing, OriginFix::Add),
        ("bare", &bare, OriginRemote::NoUrl, OriginFix::SetUrl),
    ] {
        let e = find_entry(&entries, key);
        // no fetch ran: no error from it, and no FETCH_HEAD written
        assert_eq!(e.fetch_error, None, "{key}");
        assert!(!repo.join(".git/FETCH_HEAD").exists(), "{key}");
        let expected = support::owned_origin(key);
        assert!(
            e.needs_human.contains(&NeedsHuman::OriginMismatch {
                origin,
                expected: expected.clone(),
                fix,
            }),
            "{key}: {:?}",
            e.needs_human
        );
    }
    // each advised command works where the other would fail
    let url = support::owned_origin("gone");
    let out = ws.git_output(&gone, &["remote", "set-url", "origin", &url]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("No such remote 'origin'"));
    ws.git(&gone, &["remote", "add", "origin", &url]);
    let url = support::owned_origin("bare");
    let out = ws.git_output(&bare, &["remote", "add", "origin", &url]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("remote origin already exists"));
    ws.git(&bare, &["remote", "set-url", "origin", &url]);
}

#[test]
fn a_remote_that_is_gone_is_repo_not_found() {
    let mut ws = FixtureWorkspace::new();
    ws.owned_repo("app", &[]);
    std::fs::remove_dir_all(ws.bare("app")).unwrap();

    let e = support::take_entry(ws.status_with_fetch(), "app");
    let Some(RemoteFailure::RepoNotFound { message }) = &e.fetch_error else {
        panic!("{:?}", e.fetch_error);
    };
    // it reached for the registry's repo, nothing else
    assert_eq!(message, "ERROR: Repository not found.");
    let log = ws.ssh_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0].ends_with(&format!("git@github.com git-upload-pack '{OWNER}/app'")),
        "{log:?}"
    );
    // the local probe still ran
    assert_eq!(e.probe_error, None);
    assert_eq!(e.presence, Presence::Present);
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
}

/// A fake `ssh`: records its arguments to `<name>.args` beside it, then
/// plays the failure its first argument names, with ssh's (or, for
/// `not_found`, GitHub's) lines as captured from the real thing.
const FAKE_SSH: &str = r#"#!/bin/sh
case="$1"
shift
echo "$@" > "$(dirname "$0")/$case.args"
case "$case" in
dns)
	echo "ssh: Could not resolve hostname nonexistent.invalid: Name or service not known" >&2
	exit 255 ;;
refused)
	echo "ssh: connect to host 127.0.0.1 port 1: Connection refused" >&2
	exit 255 ;;
host_key)
	echo "No ED25519 host key is known for github.com and you have requested strict checking." >&2
	echo "Host key verification failed." >&2
	exit 255 ;;
auth)
	echo "git@github.com: Permission denied (publickey)." >&2
	exit 255 ;;
not_found)
	echo "ERROR: Repository not found." >&2
	exit 1 ;;
banner)
	echo "Welcome to the server"
	exit 0 ;;
esac
exit 2
"#;

#[test]
fn ssh_failures_are_classified() {
    let mut ws = FixtureWorkspace::new();
    support::write_executable(ws.base(), "ssh/fake-ssh", FAKE_SSH);
    let fake = ws.base().join("ssh/fake-ssh");
    let cases = ["dns", "refused", "host_key", "auth", "not_found", "banner"];
    for case in cases {
        let repo = ws.owned_repo(case, &[]);
        // fetches go over SSH to the registry's host, where the repo's own
        // ssh is the fake
        let ssh = format!("'{}' {case}", fake.display());
        ws.git(&repo, &["config", "core.sshCommand", &ssh]);
        assert_eq!(
            ws.git(&repo, &["ls-remote", "--get-url", "origin"]),
            support::owned_origin(case)
        );
    }
    // the fixture allows only `file`; these fetches need `ssh` too. And a
    // fake `ssh` first on `PATH`, so even a fetch that ignored the repo's
    // `core.sshCommand` (batch-mode `ssh -o …`) never reaches a real host
    support::write_executable(
        ws.base(),
        "bin/ssh",
        &format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho 'blocked: the real ssh' >&2\nexit 255\n",
            ws.base().join("ssh/path-ssh.args").display()
        ),
    );
    let mut env: Vec<(OsString, OsString)> =
        ws.env().into_iter().filter(|(k, _)| k != "PATH").collect();
    let path = std::env::join_paths(std::iter::once(ws.base().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    env.push(("PATH".into(), path));
    env.push(("GIT_ALLOW_PROTOCOL".into(), "file:ssh".into()));
    let git = Git::with_clean_env(env);
    let entries = ws.status_with(&ws.root(), true, &git, &ws.visibility_base());

    let unreachable = |cause, message: &str| RemoteFailure::Unreachable {
        cause,
        message: message.into(),
    };
    let want = [
        unreachable(
            UnreachableCause::Dns,
            "ssh: Could not resolve hostname nonexistent.invalid: Name or service not known",
        ),
        unreachable(
            UnreachableCause::Connection,
            "ssh: connect to host 127.0.0.1 port 1: Connection refused",
        ),
        unreachable(
            UnreachableCause::HostKey,
            "No ED25519 host key is known for github.com and you have requested strict \
             checking.",
        ),
        unreachable(
            UnreachableCause::Auth,
            "git@github.com: Permission denied (publickey).",
        ),
        RemoteFailure::RepoNotFound {
            message: "ERROR: Repository not found.".into(),
        },
        // git's own line, about the banner the fake printed on stdout
        RemoteFailure::Failed {
            message: "fatal: protocol error: bad line length character: Welc".into(),
        },
    ];
    // every fetch went through the repo's own fake; the `PATH` one never ran
    assert!(!ws.base().join("ssh/path-ssh.args").exists());
    for (case, want) in cases.iter().zip(want) {
        let e = find_entry(&entries, case);
        assert_eq!(e.fetch_error.as_ref(), Some(&want), "{case}");
        assert_eq!(e.probe_error, None, "{case}");
        // the fetch reached the fake, for the registry's host and repo
        let args = std::fs::read_to_string(ws.base().join(format!("ssh/{case}.args"))).unwrap();
        assert!(
            args.contains("git@github.com") && args.contains(&format!("'{OWNER}/{case}'")),
            "{case}: {args}"
        );
    }
}
