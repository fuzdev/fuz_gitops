//! The visibility check's anonymous read, against `file://` repos and a local
//! HTTP server that demands credentials. Nothing leaves the machine.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used)]

mod support;

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use fuz_repos::git::Git;
use fuz_repos::remote::{RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::state::Presence;
use support::{FixtureWorkspace, OWNER, find_entry};

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
        ("GIT_ALLOW_PROTOCOL".into(), "file:ssh:http".into()),
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
    // the fetch itself, over the fixture's `ssh`, went on as ever
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
