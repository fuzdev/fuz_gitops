//! What `--fetch` learns from remotes: each fetch failure classified from
//! the stderr of a real git — SSH ones through a fake `ssh` that prints
//! ssh's real failure lines — and the visibility check's anonymous read,
//! against `file://` repos and a local HTTP server that demands
//! credentials. Nothing leaves the machine.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::git::Git;
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
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
    // it reached for the local bare remote, nothing else
    assert!(
        message.contains(ws.bare("app").to_str().unwrap()),
        "{message}"
    );
    assert!(
        message.ends_with("does not appear to be a git repository"),
        "{message}"
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
        // fetches go over SSH to the registry's host: no more rewrite to the
        // local bare remote, and the repo's own ssh is the fake
        let rewrite = format!("url.file://{}.insteadOf", ws.bare(case).display());
        ws.git(&repo, &["config", "--unset", &rewrite]);
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

// --- the visibility check ---

#[test]
fn a_private_repo_anyone_can_read_is_a_leak() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("leaky", &[]);
    ws.declare_repo_as("leaky", "leaky", "private", "");
    ws.clone_owned("leaky", "leaky", &[]);
    // declared private and never cloned: the check reads the host alone
    ws.remote("absent", &[]);
    ws.declare_repo_as("absent", "absent", "private", "");
    // declared public: never checked, readable or not
    ws.remote("open", &[]);
    ws.declare_repo("open", "open", "");
    ws.clone_owned("open", "open", &[]);
    for name in ["leaky", "absent", "open"] {
        ws.publish_anonymously(name);
    }
    assert!(!ws.dir("absent").exists());

    let entries = ws.status_with_fetch();
    assert_eq!(
        find_entry(&entries, "leaky").visibility_check,
        Some(VisibilityCheck::Leak)
    );
    let absent = find_entry(&entries, "absent");
    assert_eq!(absent.presence, Presence::Missing);
    assert_eq!(absent.visibility_check, Some(VisibilityCheck::Leak));
    assert_eq!(find_entry(&entries, "open").visibility_check, None);

    // no `--fetch`, no check
    for e in ws.status() {
        assert_eq!(e.visibility_check, None, "{}", e.key);
    }
}

#[test]
fn a_private_repo_nobody_can_find_is_private() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("sealed", &[]);
    ws.declare_repo_as("sealed", "sealed", "private", "");
    ws.clone_owned("sealed", "sealed", &[]);
    // a third-party reference declares no visibility: never checked
    ws.remote("lib", &[]);
    ws.declare_reference("lib", support::THIRD_PARTY, "lib", "");
    ws.clone_third_party("lib", "lib", &[]);
    assert!(!ws.anonymous_dir().exists());

    let entries = ws.status_with_fetch();
    let sealed = find_entry(&entries, "sealed");
    // the host answers as for a repo that doesn't exist
    assert_eq!(sealed.visibility_check, Some(VisibilityCheck::Private));
    assert_eq!(sealed.fetch_error, None);
    assert_eq!(find_entry(&entries, "lib").visibility_check, None);
}

/// A local HTTP server that answers every request `401` with a Basic
/// challenge, recording each request's `Authorization` headers, joined
/// (`None` when it sent none).
fn challenger() -> (u16, Arc<Mutex<Vec<Option<String>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    // lives until the test binary exits
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(read) = stream.try_clone() else {
                continue;
            };
            let mut auth = Vec::new();
            for line in BufReader::new(read).lines() {
                let Ok(line) = line else { break };
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    auth.push(value.trim().to_owned());
                }
            }
            log.lock()
                .unwrap()
                .push((!auth.is_empty()).then(|| auth.join(" | ")));
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (port, seen)
}

/// A local HTTP proxy that answers every request `407` with a Basic
/// challenge, counting the requests it saw.
fn proxy_challenger() -> (u16, Arc<Mutex<u32>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(0));
    let count = Arc::clone(&seen);
    // lives until the test binary exits
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(read) = stream.try_clone() else {
                continue;
            };
            for line in BufReader::new(read).lines() {
                match line {
                    Ok(line) if !line.is_empty() => {}
                    _ => break,
                }
            }
            *count.lock().unwrap() += 1;
            let _ = stream.write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"proxy\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (port, seen)
}

#[test]
fn a_proxy_refusal_never_reads_as_private() {
    let mut ws = FixtureWorkspace::new();
    ws.declare_repo_as("sealed", "sealed", "private", "");
    let (port, seen) = proxy_challenger();
    // a host only the proxy could reach: nothing leaves the machine
    let base = "http://repo.invalid/";
    for (proxy, requests) in [
        // a proxy user with no password: git asks for one before connecting
        (format!("http://user@127.0.0.1:{port}"), 0),
        // no proxy user: the proxy's 407 comes back
        (format!("http://127.0.0.1:{port}"), 1),
    ] {
        *seen.lock().unwrap() = 0;
        let mut env = ws.env();
        env.push(("GIT_ALLOW_PROTOCOL".into(), "file:http".into()));
        env.push(("http_proxy".into(), proxy.clone().into()));
        let e = support::take_entry(
            ws.status_with(&ws.root(), true, &Git::with_clean_env(env), base),
            "sealed",
        );
        // the host never answered: not private, whatever the proxy said
        let Some(VisibilityCheck::Unknown { failure }) = &e.visibility_check else {
            panic!("{proxy}: {:?}", e.visibility_check);
        };
        if requests == 0 {
            assert!(
                matches!(
                    failure,
                    RemoteFailure::Unreachable {
                        cause: UnreachableCause::Auth,
                        message,
                    } if message.contains(&format!("'http://user@127.0.0.1:{port}'"))
                ),
                "{failure:?}"
            );
        }
        assert_eq!(*seen.lock().unwrap(), requests, "{proxy}");
    }
}

/// A shell snippet that appends `what` to the marker file `marker`.
fn mark(marker: &Path, what: &str) -> String {
    format!("echo {what} >> '{}'", marker.display())
}

#[test]
fn the_anonymous_read_offers_no_credential() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("sealed", &[]);
    ws.declare_repo_as("sealed", "sealed", "private", "");
    ws.clone_owned("sealed", "sealed", &[]);
    let (port, seen) = challenger();
    let base = format!("http://127.0.0.1:{port}/");
    let url = format!("{base}{OWNER}/sealed");

    // every way a credential could reach the read, each leaving a mark; the
    // helpers answer nothing, so git asks each in turn, then the askpass
    let marker = ws.outside("marker");
    let helper = |what: &str| format!("!f() {{ {}; }}; f", mark(&marker, what));
    let global = ws.outside("global.gitconfig");
    let system = ws.outside("system.gitconfig");
    ws.git(
        ws.base(),
        &[
            "config",
            "-f",
            global.to_str().unwrap(),
            "credential.helper",
            &helper("global"),
        ],
    );
    let scoped = format!("credential.http://127.0.0.1:{port}.helper");
    ws.git(
        ws.base(),
        &[
            "config",
            "-f",
            global.to_str().unwrap(),
            &scoped,
            &helper("scoped"),
        ],
    );
    ws.git(
        ws.base(),
        &[
            "config",
            "-f",
            global.to_str().unwrap(),
            "http.extraHeader",
            "Authorization: Basic ZXh0cmE6aGVhZGVy",
        ],
    );
    ws.git(
        ws.base(),
        &[
            "config",
            "-f",
            system.to_str().unwrap(),
            "credential.helper",
            &helper("system"),
        ],
    );
    // a header no emptied helper list stops, from each config source
    let header = |who: &str| format!("Authorization: Basic {who}");
    ws.git(
        ws.base(),
        &[
            "config",
            "-f",
            system.to_str().unwrap(),
            "http.extraHeader",
            &header("system"),
        ],
    );
    // the workspace root is itself a repo with a helper and header of its own
    ws.git(&ws.root(), &["init", "-q"]);
    ws.git(
        &ws.root(),
        &["config", "credential.helper", &helper("root-repo")],
    );
    ws.git(
        &ws.root(),
        &["config", "http.extraHeader", &header("root-repo")],
    );
    let home = ws.outside("credhome");
    support::write(&home, ".netrc", "machine 127.0.0.1 login nu password np\n");
    support::write_executable(
        ws.base(),
        "askpass",
        &format!("#!/bin/sh\n{}\necho secret\n", mark(&marker, "askpass")),
    );
    let askpass = ws.base().join("askpass");
    let mut env: Vec<(OsString, OsString)> = ws
        .env()
        .into_iter()
        .filter(|(k, _)| k != "HOME" && k != "GIT_CONFIG_GLOBAL" && k != "GIT_CONFIG_NOSYSTEM")
        .collect();
    env.extend([
        ("HOME".into(), home.into()),
        ("GIT_CONFIG_GLOBAL".into(), global.into()),
        ("GIT_CONFIG_SYSTEM".into(), system.into()),
        ("GIT_CONFIG_COUNT".into(), "2".into()),
        ("GIT_CONFIG_KEY_0".into(), "credential.helper".into()),
        ("GIT_CONFIG_VALUE_0".into(), helper("env").into()),
        ("GIT_CONFIG_KEY_1".into(), "http.extraHeader".into()),
        ("GIT_CONFIG_VALUE_1".into(), header("env").into()),
        // what `git -c` hands its children
        (
            "GIT_CONFIG_PARAMETERS".into(),
            format!("'http.extraheader'='{}'", header("params")).into(),
        ),
        ("GIT_ASKPASS".into(), askpass.clone().into()),
        ("SSH_ASKPASS".into(), askpass.into()),
        ("GIT_ALLOW_PROTOCOL".into(), "file:http".into()),
    ]);

    // control: plain git, from the workspace root, hands them over
    let out = std::process::Command::new("git")
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(ws.root())
        .args(["ls-remote", &url, "HEAD"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let marks = std::fs::read_to_string(&marker).unwrap_or_default();
    for source in ["system", "global", "scoped", "root-repo", "env", "askpass"] {
        assert!(
            marks.contains(source),
            "control: {source} didn't run: {marks}"
        );
    }
    let sent = seen.lock().unwrap().clone();
    for who in ["system", "root-repo", "env", "params"] {
        assert!(
            sent.iter()
                .flatten()
                .any(|a| a.contains(&format!("Basic {who}"))),
            "control: {who}'s header wasn't sent: {sent:?}"
        );
    }
    std::fs::remove_file(&marker).unwrap();
    seen.lock().unwrap().clear();

    let git = Git::with_clean_env(env);
    let e = support::take_entry(ws.status_with(&ws.root(), true, &git, &base), "sealed");
    // refused, as a private repo's anonymous read is
    assert_eq!(e.visibility_check, Some(VisibilityCheck::Private));
    let requests = seen.lock().unwrap().clone();
    assert!(!requests.is_empty(), "the check reached the server");
    assert!(
        requests.iter().all(Option::is_none),
        "credentials were sent: {requests:?}"
    );
    assert!(
        !marker.exists(),
        "a credential source ran: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
    // the fetch itself, over `file://`, went on as ever
    assert_eq!(e.fetch_error, None);

    // credentials in the URL itself: refused before anything is sent, and
    // never repeated
    seen.lock().unwrap().clear();
    let with_userinfo = format!("http://user:sekrit@127.0.0.1:{port}/");
    let e = support::take_entry(
        ws.status_with(&ws.root(), true, &git, &with_userinfo),
        "sealed",
    );
    let Some(VisibilityCheck::Unknown {
        failure: RemoteFailure::Failed { message },
    }) = &e.visibility_check
    else {
        panic!("{:?}", e.visibility_check);
    };
    assert!(message.contains("without credentials"), "{message}");
    assert!(
        !message.contains("sekrit") && !message.contains("user"),
        "{message}"
    );
    assert!(seen.lock().unwrap().is_empty(), "the read was sent");
    assert!(!marker.exists());
}

#[test]
fn a_check_that_cannot_tell_is_unknown() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("dark", &[]);
    ws.declare_repo_as("dark", "dark", "private", "");
    ws.clone_owned("dark", "dark", &[]);
    // port 1 (tcpmux), which nothing serves here: a bound-then-dropped
    // ephemeral port could be taken again before git connects
    let base = "http://127.0.0.1:1/";

    let mut env = ws.env();
    env.push(("GIT_ALLOW_PROTOCOL".into(), "file:http".into()));
    let e = support::take_entry(
        ws.status_with(&ws.root(), true, &Git::with_clean_env(env), base),
        "dark",
    );
    let Some(VisibilityCheck::Unknown {
        failure:
            RemoteFailure::Unreachable {
                cause: UnreachableCause::Connection,
                message,
            },
    }) = &e.visibility_check
    else {
        panic!("{:?}", e.visibility_check);
    };
    assert!(
        message.contains("Failed to connect to 127.0.0.1"),
        "{message}"
    );

    // the caller's protocol allowlist stands: the fixture's allows only
    // `file`, so git refuses the read before it leaves the process
    let e = support::take_entry(ws.status_with(&ws.root(), true, &ws.runner(), base), "dark");
    assert_eq!(
        e.visibility_check,
        Some(VisibilityCheck::Unknown {
            failure: RemoteFailure::Failed {
                message: "fatal: transport 'http' not allowed".into()
            }
        })
    );
}

// --- what the advice can reach: config scopes, URL lists ---

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

/// An entry's origin drift, if any.
fn drift(e: &fuz_repos::report::EntryStatus) -> Option<(OriginRemote, OriginFix)> {
    e.needs_human.iter().find_map(|r| match r {
        NeedsHuman::OriginMismatch { origin, fix, .. } => Some((origin.clone(), fix.clone())),
        _ => None,
    })
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
    let error = e.probe_error.as_deref().unwrap_or_default();
    assert!(
        error.contains("missing value for 'remote.origin.url'"),
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

// --- what the fetch may write: remote-tracking refs, nothing else ---

/// Whether plain `git fetch --prune origin` in `repo`, as a person would run
/// it, succeeds — the control each guard below is measured against.
fn plain_fetch(ws: &FixtureWorkspace, repo: &Path) -> bool {
    ws.git_output(
        repo,
        &[
            "-c",
            "maintenance.auto=false",
            "fetch",
            "-q",
            "--prune",
            "origin",
        ],
    )
    .status
    .success()
}

#[test]
fn the_fetch_never_prunes_or_follows_tags() {
    let mut ws = FixtureWorkspace::new();
    for name in ["prune", "rprune", "follow", "tagopt", "control"] {
        ws.owned_repo(name, &[]);
    }
    for (name, key) in [
        ("prune", "fetch.pruneTags"),
        ("rprune", "remote.origin.pruneTags"),
        ("control", "fetch.pruneTags"),
    ] {
        let repo = ws.dir(name);
        ws.git(&repo, &["tag", "v9-local-only"]);
        ws.git(&repo, &["config", key, "true"]);
    }
    ws.git(
        &ws.dir("tagopt"),
        &["config", "remote.origin.tagOpt", "--tags"],
    );
    // a new commit and tag upstream for every repo
    for name in ["prune", "rprune", "follow", "tagopt", "control"] {
        ws.upstream_commit(name, "main");
        let up = ws.upstream(name);
        ws.git(&up, &["tag", "v2"]);
        ws.git(&up, &["push", "-q", "origin", "v2"]);
    }
    // control: a plain fetch deletes the unpushed tag and follows the new one
    let control = ws.dir("control");
    assert!(plain_fetch(&ws, &control));
    assert_eq!(ws.git(&control, &["tag"]), "v2");

    let entries = ws.status_with_fetch();
    for (name, tags) in [
        ("prune", "v9-local-only"),
        ("rprune", "v9-local-only"),
        ("follow", ""),
        ("tagopt", ""),
    ] {
        let e = find_entry(&entries, name);
        assert_eq!(e.fetch_error, None, "{name}");
        // the fetch ran: the branch reads behind
        assert_eq!(
            branch(e, "main").relation,
            Relation::Behind { commits: 1 },
            "{name}"
        );
        assert_eq!(ws.git(&ws.dir(name), &["tag"]), tags, "{name}");
    }
}

#[test]
fn the_fetch_never_recurses_into_submodules() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("sub", &[]);
    ws.remote("app", &[]);
    let up = ws.upstream("app");
    let sub_url = format!("file://{}", ws.bare("sub").display());
    ws.git(
        &up,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub_url,
            "sub",
        ],
    );
    ws.git(&up, &["commit", "-q", "-m", "add sub"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    // a second clone for the control, never registered: once a fetch has
    // brought the new commits, a later one has nothing to recurse for
    let control = ws.clone_owned("control", "app", &[]);
    for repo in [&app, &control] {
        ws.git(
            repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "-q",
                "--init",
            ],
        );
    }
    let modules = app.join(".git/modules/sub");
    assert!(modules.join("HEAD").is_file());
    // the submodule moves upstream, the parent's pointer with it, and then
    // the submodule's URL goes away
    ws.upstream_commit("sub", "main");
    ws.git(&up.join("sub"), &["pull", "-q", "origin", "main"]);
    ws.git(&up, &["commit", "-q", "-am", "bump sub"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    std::fs::rename(ws.bare("sub"), ws.outside("sub-moved.git")).unwrap();
    // control: a plain fetch recurses, and fails for the submodule
    let out = ws.git_output(
        &control,
        &[
            "-c",
            "maintenance.auto=false",
            "fetch",
            "-q",
            "--prune",
            "origin",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Errors during submodule fetch"));
    let before = support::snapshot_git_dir(&modules);

    let e = support::take_entry(ws.status_with_fetch(), "app");
    // the entry's own fetch ran, and is all that ran
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::Behind { commits: 1 });
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&modules));
}

#[test]
fn the_fetch_writes_no_commit_graph_and_no_bundles() {
    let mut ws = FixtureWorkspace::new();
    for name in ["graph", "graph_control", "bundle", "bundle_control"] {
        ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
    }
    for name in ["graph", "graph_control"] {
        ws.git(&ws.dir(name), &["config", "fetch.writeCommitGraph", "true"]);
    }
    for name in ["bundle", "bundle_control"] {
        let bundle = ws.outside(&format!("{name}.bundle"));
        ws.git(
            &ws.upstream(name),
            &["bundle", "create", "-q", bundle.to_str().unwrap(), "main"],
        );
        let repo = ws.dir(name);
        let url = format!("file://{}", bundle.display());
        ws.git(&repo, &["config", "fetch.bundleURI", &url]);
    }
    let graph = |name: &str| {
        let info = ws.dir(name).join(".git/objects/info");
        ["commit-graph", "commit-graphs"]
            .iter()
            .any(|f| info.join(f).exists())
    };
    let bundles = |name: &str| {
        ws.git(&ws.dir(name), &["for-each-ref", "refs/bundles"])
            .lines()
            .count()
    };
    assert!(!graph("graph") && bundles("bundle") == 0);
    // controls: a plain fetch writes both
    assert!(plain_fetch(&ws, &ws.dir("graph_control")));
    assert!(graph("graph_control"));
    assert!(plain_fetch(&ws, &ws.dir("bundle_control")));
    assert!(bundles("bundle_control") > 0);

    let entries = ws.status_with_fetch();
    for name in ["graph", "bundle"] {
        let e = find_entry(&entries, name);
        assert_eq!(e.fetch_error, None, "{name}");
        assert_eq!(
            branch(e, "main").relation,
            Relation::Behind { commits: 1 },
            "{name}"
        );
    }
    assert!(!graph("graph"));
    assert_eq!(bundles("bundle"), 0);
}

#[test]
fn a_refspec_writing_outside_remote_tracking_refs_is_not_fetched() {
    let mut ws = FixtureWorkspace::new();
    for name in ["tags", "mirror"] {
        ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        let up = ws.upstream(name);
        ws.git(&up, &["tag", "v2"]);
        ws.git(&up, &["push", "-q", "origin", "v2"]);
    }
    let tags = ws.dir("tags");
    ws.git(
        &tags,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    );
    let mirror = ws.dir("mirror");
    ws.git(&mirror, &["checkout", "-q", "--detach"]);
    ws.git(
        &mirror,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ],
    );
    let main_before = ws.git(&mirror, &["rev-parse", "main"]);

    let entries = ws.status_with_fetch();
    for (name, refspec) in [
        ("tags", "+refs/tags/*:refs/tags/*"),
        ("mirror", "+refs/heads/*:refs/heads/*"),
    ] {
        let e = find_entry(&entries, name);
        assert_eq!(
            e.fetch_error,
            Some(RemoteFailure::RefspecOutsideOrigin {
                refspec: refspec.into()
            }),
            "{name}"
        );
        assert!(!ws.dir(name).join(".git/FETCH_HEAD").exists(), "{name}");
    }
    assert_eq!(ws.git(&tags, &["tag"]), "");
    assert_eq!(ws.git(&mirror, &["rev-parse", "main"]), main_before);
    // control: a plain fetch writes both
    assert!(plain_fetch(&ws, &tags));
    assert_eq!(ws.git(&tags, &["tag"]), "v2");
    assert!(plain_fetch(&ws, &mirror));
    assert_ne!(ws.git(&mirror, &["rev-parse", "main"]), main_before);
}

#[test]
fn a_refspec_writing_into_another_remotes_refs_is_not_fetched() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("other", &[]);
    ws.upstream_commit("other", "only-upstream");
    let other_url = format!("file://{}", ws.bare("other").display());
    let cases = [
        ("into_upstream", "+refs/heads/*:refs/remotes/upstream/*"),
        ("into_all", "+refs/heads/*:refs/remotes/*"),
    ];
    for (name, refspec) in cases {
        let repo = ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        ws.git(&repo, &["remote", "add", "upstream", &other_url]);
        ws.git(&repo, &["fetch", "-q", "upstream"]);
        ws.git(&repo, &["config", "--add", "remote.origin.fetch", refspec]);
    }
    let remote_refs = |repo: &Path| ws.git(repo, &["for-each-ref", "refs/remotes"]);
    let before: Vec<String> = cases.iter().map(|(n, _)| remote_refs(&ws.dir(n))).collect();
    for b in &before {
        assert!(b.contains("refs/remotes/upstream/only-upstream"), "{b}");
    }

    let entries = ws.status_with_fetch();
    for ((name, refspec), before) in cases.iter().zip(&before) {
        assert_eq!(
            find_entry(&entries, name).fetch_error,
            Some(RemoteFailure::RefspecOutsideOrigin {
                refspec: (*refspec).into()
            }),
            "{name}"
        );
        // another remote's tracking refs untouched
        assert_eq!(&remote_refs(&ws.dir(name)), before, "{name}");
    }
    // control: a plain fetch clobbers them, pruning `upstream`'s own branch
    for (name, _) in cases {
        let repo = ws.dir(name);
        assert!(plain_fetch(&ws, &repo));
        assert!(
            !remote_refs(&repo).contains("refs/remotes/upstream/only-upstream"),
            "{name}"
        );
    }
}

#[test]
fn a_remote_nested_under_origin_is_not_pruned_away() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("fork", &[]);
    ws.upstream_commit("fork", "feat");
    let fork_url = format!("file://{}", ws.bare("fork").display());
    for name in ["app", "control"] {
        let repo = ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        // a remote named `origin/fork`: its refs land under origin's
        ws.git(&repo, &["remote", "add", "origin/fork", &fork_url]);
        ws.git(&repo, &["fetch", "-q", "origin/fork"]);
    }
    let nested = |repo: &Path| {
        ws.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/remotes/origin/fork",
            ],
        )
    };
    let app = ws.dir("app");
    let before = nested(&app);
    assert_eq!(
        before,
        "refs/remotes/origin/fork/feat\nrefs/remotes/origin/fork/main"
    );
    // control: a plain pruning fetch of origin deletes them
    let control = ws.dir("control");
    assert!(plain_fetch(&ws, &control));
    assert_eq!(nested(&control), "");

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(
        e.fetch_error,
        Some(RemoteFailure::OriginRefsShared {
            remote: "origin/fork".into(),
            refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
        })
    );
    assert_eq!(nested(&app), before);
    // origin's own view isn't the other remote's: no drift
    assert!(drift(&e).is_none());
}

#[test]
fn a_glob_or_legacy_remote_under_origin_is_not_pruned_away() {
    let mut ws = FixtureWorkspace::new();
    // a fork with branches whose names a `*` carries under origin's, and a
    // tracking ref of its own at `refs/remotes/origin/z`
    ws.remote("fork", &[]);
    ws.upstream_commit("fork", "fx");
    ws.upstream_commit("fork", "origin/y");
    let up = ws.upstream("fork");
    ws.git(&up, &["update-ref", "refs/remotes/origin/z", "HEAD"]);
    ws.git(&up, &["push", "-q", "origin", "refs/remotes/origin/z"]);
    let fork_url = format!("file://{}", ws.bare("fork").display());
    // (entry, refspec, legacy `Pull:` line rather than config, the ref it
    // shares with origin)
    let cases = [
        (
            "glob_origin",
            "+refs/heads*:refs/remotes/origin*",
            false,
            "refs/remotes/origin/fx",
        ),
        (
            "glob_remotes",
            "+refs/heads/*:refs/remotes/*",
            false,
            "refs/remotes/origin/y",
        ),
        ("glob_refs", "+refs*:refs*", false, "refs/remotes/origin/z"),
        ("glob_ref", "+ref*:ref*", false, "refs/remotes/origin/z"),
        (
            "legacy",
            "+refs/heads/fx:refs/remotes/origin/fork-fx",
            true,
            "refs/remotes/origin/fork-fx",
        ),
        ("legacy_glob", "+ref*:ref*", true, "refs/remotes/origin/z"),
    ];
    for (name, refspec, legacy, _) in cases {
        for dir in [name.to_owned(), format!("{name}_control")] {
            let repo = if dir == name {
                ws.owned_repo(name, &[])
            } else {
                ws.clone_owned(&dir, name, &[])
            };
            // a full-name glob would fetch into the checked-out branch
            ws.git(&repo, &["checkout", "-q", "--detach"]);
            let remote = if legacy {
                support::write(
                    &repo,
                    ".git/remotes/legacy",
                    &format!("URL: {fork_url}\nPull: {refspec}\n"),
                );
                "legacy"
            } else {
                ws.git(&repo, &["remote", "add", "fork", &fork_url]);
                ws.git(
                    &repo,
                    &["config", "--replace-all", "remote.fork.fetch", refspec],
                );
                "fork"
            };
            ws.git(&repo, &["fetch", "-q", remote]);
        }
    }
    let has = |repo: &Path, r: &str| ws.has_ref(repo, r);
    for (name, _, _, shared) in cases {
        assert!(has(&ws.dir(name), shared), "{name}");
        // control: a plain pruning fetch of origin deletes it
        let control = ws.dir(&format!("{name}_control"));
        assert!(has(&control, shared), "{name}");
        assert!(plain_fetch(&ws, &control));
        assert!(!has(&control, shared), "control {name}");
    }

    let entries = ws.status_with_fetch();
    for (name, refspec, legacy, shared) in cases {
        let e = find_entry(&entries, name);
        let remote = if legacy { "legacy" } else { "fork" };
        assert_eq!(
            e.fetch_error,
            Some(RemoteFailure::OriginRefsShared {
                remote: remote.into(),
                refspec: refspec.into(),
            }),
            "{name}"
        );
        assert!(has(&ws.dir(name), shared), "{name}");
    }
}

#[test]
fn a_legacy_origin_file_beside_a_configured_origin_is_ignored() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    // git reads `remotes/origin` only when config gives origin no URL
    support::write(
        &app,
        ".git/remotes/origin",
        "URL: file:///nowhere\nPull: +refs/heads/*:refs/remotes/origin/*\n",
    );
    // control: git ignores it (its URL goes nowhere, and the fetch works)
    assert!(plain_fetch(&ws, &app));
    ws.upstream_commit("app", "main");

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::Behind { commits: 2 });
}
