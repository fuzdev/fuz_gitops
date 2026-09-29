//! Calls that reach a remote: why one failed, classified from git's stderr,
//! and the visibility check — an anonymous read of each repo declared
//! private.
//!
//! Classification is pure over the text git and its transports (ssh, curl)
//! print under `LC_ALL=C`, which the runner sets, plus the fetch refspecs a
//! missing ref is repaired against. `unreachable`, `repo_not_found`, and
//! `failed` carry the line that decided them, verbatim; `ref_gone` carries
//! the ref git named and the repair decided for it, `timed_out` the
//! runner's timeout. A renderer words each kind and may add a hint, but the
//! kind and any repair are decided here.

use std::path::Path;

use serde::Serialize;

use crate::git::{Git, GitError};
use crate::porcelain::ConfigValue;
use crate::registry::{Entry, EntryKind, Visibility};
use crate::url::{escape_ere, url_origin};

/// Why a call to a remote failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteFailure {
    /// The fetch names a ref the remote doesn't have, so git fetched
    /// nothing: a fetch refspec narrowed to a branch (a single-branch clone,
    /// or `git remote set-branches`) that was deleted or renamed on the
    /// remote. The upstream is gone; the host answered.
    RefGone {
        /// The ref as git names it, e.g. `refs/heads/feat` — the source side
        /// of the refspec that names it.
        refname: String,
        fix: RefGoneFix,
    },
    /// The host couldn't be reached, or wouldn't let this client in.
    Unreachable {
        cause: UnreachableCause,
        /// Git's (or its transport's) line that names the cause.
        message: String,
    },
    /// The host has no repo at the URL, or hides it from this client:
    /// GitHub answers a private repo the client can't read the same way as
    /// one that doesn't exist.
    RepoNotFound { message: String },
    /// The runner's timeout fired, and git was stopped. Distinct from a
    /// connect timeout, which is `Unreachable` with cause `connection`.
    TimedOut { after_secs: u64 },
    /// Anything else, with git's first line.
    Failed { message: String },
    /// Not a remote's answer: the fetch wasn't run, because origin's
    /// `refspec` writes outside `refs/remotes/origin/` (a local branch, a
    /// tag, another remote's tracking refs), and `status --fetch` writes
    /// origin's remote-tracking refs only.
    RefspecOutsideOrigin { refspec: String },
    /// Not a remote's answer: the fetch wasn't run, because another remote,
    /// `remote`, has a `refspec` whose destination may lie under
    /// `refs/remotes/origin/`, where origin's `--prune` would delete what it
    /// wrote: read as git reads it — a `*` destination substituted as
    /// written, so any whose text before the `*` shares a prefix with
    /// `refs/remotes/origin/` (`*`, `refs*`, `refs/remotes/*`,
    /// `refs/remotes/origin*`); a plain one after git's DWIM (a remote named
    /// `origin/<x>`). A legacy remote (a file under `remotes/`) counts, by
    /// its file name.
    OriginRefsShared { remote: String, refspec: String },
    /// Not a remote's answer: the fetch wasn't run, because a legacy remotes
    /// dir, or a file in it, at `path` couldn't be read, and a remote there
    /// may have written under `refs/remotes/origin/`.
    LegacyRemotesUnreadable { path: String },
}

/// How to stop a fetch refspec that names a ref the remote no longer has
/// from failing every fetch.
///
/// Without making the rest of the repo's fetch worse: never `git remote
/// set-branches` while other refspecs stand, since it replaces every one of
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefGoneFix {
    /// Drop just the refspecs naming it, keeping the rest: `git config
    /// --unset-all remote.origin.fetch <pattern>`. `pattern` is the extended
    /// regex git matches each value against: the ref, escaped, after an
    /// optional `+`, before `:` or the value's end.
    UnsetRefspec { pattern: String },
    /// No positive refspec in the repo's config file would remain once the
    /// ones naming it went, and a fetch left with none (or with only
    /// negative ones, or another scope's) succeeds while updating no branch —
    /// stale refs that look fetched — so point origin at a branch the remote
    /// has instead: `git remote set-branches origin <branch>`. `branch` is
    /// the registry's branch when it isn't the one gone; `None` leaves the
    /// choice to the reader.
    SetBranches { branch: Option<String> },
    /// A refspec naming it lives outside the repo's own config file (global
    /// or system config, a file the repo's includes, its worktree config),
    /// where `git config --unset-all` doesn't reach; or no refspec git reads
    /// names it as git does. Fix by hand.
    ByHand,
}

impl RefGoneFix {
    /// The repair for `refname` against origin's fetch refspecs, from what
    /// the advised command can change: only refspecs in the repo's own
    /// config file.
    ///
    /// A refspec naming it anywhere else is `ByHand`. The ones that would
    /// remain are counted over the repo's own file alone, and never count a
    /// negative refspec (`^refs/…`): with only those left, or only another
    /// scope's (a global `refs/pull/*` line, say), a fetch succeeds while
    /// updating no branch — so that case is `SetBranches`.
    pub fn decide(refname: &str, cx: RefspecContext<'_>) -> Self {
        let names = |v: &ConfigValue| {
            let refspec = v.value.strip_prefix('+').unwrap_or(&v.value);
            refspec.split(':').next() == Some(refname)
        };
        let naming: Vec<&ConfigValue> = cx.refspecs.iter().filter(|v| names(v)).collect();
        if naming.is_empty() || naming.iter().any(|v| !v.in_repo_file) {
            return Self::ByHand;
        }
        let remaining = cx
            .refspecs
            .iter()
            .filter(|v| v.in_repo_file && !names(v) && !v.value.starts_with('^'))
            .count();
        if remaining == 0 {
            // the registry's branch, unless it's the one that's gone
            let branch = cx
                .branch
                .filter(|b| *b != refname && format!("refs/heads/{b}") != refname)
                .map(str::to_owned);
            Self::SetBranches { branch }
        } else {
            Self::UnsetRefspec {
                pattern: format!("^\\+?{}(:|$)", escape_ere(refname)),
            }
        }
    }
}

/// What a missing ref's repair is decided against.
#[derive(Debug, Clone, Copy, Default)]
pub struct RefspecContext<'a> {
    /// Origin's fetch refspecs, as git reads them.
    pub refspecs: &'a [ConfigValue],
    /// The entry's branch, if any: what `SetBranches` names.
    pub branch: Option<&'a str>,
}

/// Why a host couldn't be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnreachableCause {
    /// Its name didn't resolve.
    Dns,
    /// No connection: refused, timed out, reset or closed, or no route.
    Connection,
    /// Its identity isn't trusted: an SSH host key unknown or changed (under
    /// batch mode ssh never asks), or an HTTPS certificate that doesn't
    /// verify.
    HostKey,
    /// It refused this client's credentials, or asked for some and got
    /// none: the SSH key isn't accepted (or, locked, couldn't be offered
    /// under batch mode), or HTTPS wanted a username.
    Auth,
}

/// Lines naming a host whose identity isn't trusted. The more specific
/// lines come first in each list, so the message names what's wrong.
const HOST_KEY: [&str; 6] = [
    // ssh: `No ED25519 host key is known for <host> and you have requested
    // strict checking.`
    " host key is known for ",
    // ssh: `Host key for <host> has changed and you have requested strict
    // checking.`, after its man-in-the-middle warning
    "Host key for ",
    "Host key verification failed",
    // curl (OpenSSL): `SSL certificate problem: self-signed certificate`
    "SSL certificate problem",
    // curl (GnuTLS): `server verification failed: certificate signer not
    // trusted.`
    "server verification failed",
    "server certificate verification failed",
];

/// Lines naming refused or missing credentials.
const AUTH: [&str; 4] = [
    // ssh: `git@github.com: Permission denied (publickey).`
    "Permission denied (",
    // HTTPS with no credential to give: `could not read Username for
    // 'https://github.com': terminal prompts disabled`
    "could not read Username for",
    "could not read Password for",
    // HTTPS with credentials the host rejected
    "Authentication failed for",
];

/// Lines naming a repo the host doesn't have (or hides).
const REPO_NOT_FOUND: [&str; 4] = [
    // GitHub over SSH `ERROR: Repository not found.`, over HTTPS `remote:
    // Repository not found.`
    "Repository not found",
    // a path with no repo, local or at the far end of SSH: `fatal:
    // '/srv/x.git' does not appear to be a git repository`
    "does not appear to be a git repository",
    // GitLab over SSH
    "The project you were looking for could not be found",
    // HTTPS 404: `fatal: repository 'https://host/x/y/' not found`
    "' not found",
];

/// Lines naming a host name that didn't resolve.
const DNS: [&str; 3] = [
    // ssh `Could not resolve hostname <h>: …`, curl `Could not resolve
    // host: <h>`
    "Could not resolve host",
    "Could not resolve proxy",
    // git://
    "unable to look up",
];

/// Lines naming a connection that failed.
const CONNECTION: [&str; 12] = [
    // ssh: `ssh: connect to host <h> port <p>: Connection refused` (or
    // `Connection timed out`, `Network is unreachable`, `No route to host`)
    "connect to host ",
    "Connection refused",
    "Connection timed out",
    "Network is unreachable",
    "No route to host",
    "Connection closed by",
    "Connection reset by",
    "kex_exchange_identification",
    // curl: `Failed to connect to <h> port <p> after 0 ms: Could not
    // connect to server`
    "Failed to connect to",
    "connect to server",
    "Operation timed out",
    // git://
    "unable to connect to",
];

impl RemoteFailure {
    /// Classifies a failed call's stderr; `cx` is what a missing ref's
    /// repair is decided against (empty for a call that fetches none).
    ///
    /// The first match wins, in this order: a missing ref, an untrusted
    /// host, refused credentials, a missing repo, a name that didn't
    /// resolve, a failed connection; else `Failed` with the first `fatal:`
    /// line, or failing that the first non-empty one (a `warning:` or trace
    /// line may come first).
    pub fn from_stderr(stderr: &str, cx: RefspecContext<'_>) -> Self {
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        // the first line holding any of `patterns`, trying them in order
        let find = |patterns: &[&str]| -> Option<String> {
            patterns.iter().find_map(|p| {
                lines
                    .iter()
                    .find(|l| l.contains(p))
                    .map(|l| (*l).to_owned())
            })
        };
        if let Some(refname) = lines.iter().find_map(|l| {
            l.split_once("couldn't find remote ref ")
                .map(|(_, r)| r.trim())
                .filter(|r| !r.is_empty())
        }) {
            return Self::RefGone {
                refname: refname.to_owned(),
                fix: RefGoneFix::decide(refname, cx),
            };
        }
        let unreachable = |cause, message| Self::Unreachable { cause, message };
        if let Some(message) = find(&HOST_KEY) {
            return unreachable(UnreachableCause::HostKey, message);
        }
        if let Some(message) = find(&AUTH) {
            return unreachable(UnreachableCause::Auth, message);
        }
        if let Some(message) = find(&REPO_NOT_FOUND) {
            return Self::RepoNotFound { message };
        }
        if let Some(message) = find(&DNS) {
            return unreachable(UnreachableCause::Dns, message);
        }
        if let Some(message) = find(&CONNECTION) {
            return unreachable(UnreachableCause::Connection, message);
        }
        let message = lines
            .iter()
            .find(|l| l.starts_with("fatal:"))
            .or_else(|| lines.first())
            .map_or_else(String::new, |l| (*l).to_owned());
        Self::Failed { message }
    }

    /// Classifies a failed network call: git's stderr when it exited
    /// non-zero (`from_stderr`, with `cx`), `TimedOut` when the runner
    /// stopped it, else `Failed` with the runner's error.
    pub fn from_git_error(e: GitError, cx: RefspecContext<'_>) -> Self {
        match e {
            GitError::Failed { stderr, code, .. } if stderr.trim().is_empty() => Self::Failed {
                message: code.map_or_else(
                    || "git was killed by a signal, with no message".to_owned(),
                    |c| format!("git exited {c} with no message"),
                ),
            },
            GitError::Failed { stderr, .. } => Self::from_stderr(&stderr, cx),
            GitError::Timeout { after, .. } => Self::TimedOut {
                after_secs: after.as_secs(),
            },
            e => Self::Failed {
                message: e.to_string(),
            },
        }
    }
}

/// What an anonymous read of a repo declared private found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VisibilityCheck {
    /// Anyone can read it: the repo is public though declared private, and
    /// the membrane lint, which skips private repos, never scanned it.
    Leak,
    /// The host refused the anonymous read — it asked for credentials, or
    /// answered as for a repo that doesn't exist — so it's private as
    /// declared (or gone, which leaks nothing).
    Private,
    /// The check couldn't tell — including a refusal that didn't come from
    /// the host (a proxy asking for a password reads `unreachable` with
    /// cause `auth`), so a check that never reached the host can't pass as
    /// private.
    Unknown { failure: RemoteFailure },
}

impl VisibilityCheck {
    /// Decides the check from the anonymous read of `url`: success is a
    /// leak; a refusal is `Private` only when git's refusal names `url`'s own
    /// origin (`refused_by_host`); anything else is `Unknown`.
    pub fn from_read(url: &str, result: Result<(), GitError>) -> Self {
        match result {
            Ok(()) => Self::Leak,
            Err(GitError::Failed { stderr, .. }) if refused_by_host(&stderr, url) => Self::Private,
            Err(e) => Self::Unknown {
                failure: RemoteFailure::from_git_error(e, RefspecContext::default()),
            },
        }
    }
}

/// Whether `stderr` holds the refusal of `url`'s own host: an auth-shaped
/// line (credentials asked for, or refused) or a not-found one that quotes
/// `url`'s origin, `'<scheme>://<authority>` followed by `'` or `/` — never
/// a longer host that merely starts the same (`github.com.evil`). For a
/// local `file://` URL, which has no host, the quoted path.
///
/// The quote is what tells the host's refusal from anyone else's: a proxy
/// that wants a password (`could not read Password for
/// 'http://user@proxy:3128'`), or a client certificate's passphrase,
/// quotes another origin, and a check that never reached the host must not
/// read as private — that would hide the very leak it looks for. GitHub
/// answers an anonymous read of a private or missing repo with `could not
/// read Username for 'https://github.com'`; other hosts with `repository
/// '<url>/' not found` or `Authentication failed for '<url>/'`.
fn refused_by_host(stderr: &str, url: &str) -> bool {
    // a local `file://` repo has no host; git quotes its path instead
    let Some(names) = url.strip_prefix("file://").or_else(|| url_origin(url)) else {
        return false;
    };
    let quoted = format!("'{names}");
    let refusal = |line: &str| AUTH.iter().chain(&REPO_NOT_FOUND).any(|p| line.contains(p));
    stderr.lines().any(|line| {
        refusal(line)
            && line
                .match_indices(&quoted)
                .any(|(i, _)| matches!(line[i + quoted.len()..].chars().next(), Some('\'' | '/')))
    })
}

/// Whether the visibility check applies to this entry: a `[repos]` entry
/// declared private. A repo declared public but really private harms
/// nothing, and references declare no visibility.
pub fn is_declared_private(entry: &Entry) -> bool {
    entry.kind == EntryKind::Repo && entry.visibility == Some(Visibility::Private)
}

/// The URL the visibility check reads: the registry's HTTPS URL, or, with
/// `base`, `<base><account>/<name>` (a test seam).
pub fn visibility_url(entry: &Entry, base: Option<&str>) -> String {
    base.map_or_else(
        || entry.url.to_string(),
        |base| format!("{base}{}/{}", entry.url.account, entry.url.name),
    )
}

/// Reads `url` anonymously (`Git::ls_remote_anonymous`) and decides what
/// that says about a repo declared private; `dir` is any existing dir to run
/// in, and no repo's config applies there.
pub fn read_anonymously(git: &Git, dir: &Path, url: &str) -> VisibilityCheck {
    VisibilityCheck::from_read(url, git.ls_remote_anonymous(dir, url))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Git's lines after a failed SSH transport, as captured.
    const COULD_NOT_READ: &str = "fatal: Could not read from remote repository.\n\n\
        Please make sure you have the correct access rights\nand the repository exists.\n";

    fn unreachable(cause: UnreachableCause, message: &str) -> RemoteFailure {
        RemoteFailure::Unreachable {
            cause,
            message: message.into(),
        }
    }

    /// Each stderr below was captured verbatim from git 2.47.3 and OpenSSH
    /// 10.0p2 (curl over `GnuTLS` for HTTPS) under `LC_ALL=C`, except
    /// GitHub's SSH `Repository not found`, which needs a key GitHub knows.
    #[test]
    fn classifies_captured_stderr() {
        let cases: Vec<(String, RemoteFailure)> = vec![
            // a single-branch clone whose branch was deleted on the remote
            (
                "fatal: couldn't find remote ref refs/heads/feat\n".into(),
                RemoteFailure::RefGone {
                    refname: "refs/heads/feat".into(),
                    fix: RefGoneFix::ByHand,
                },
            ),
            // an explicit short refspec
            (
                "fatal: couldn't find remote ref fork\n".into(),
                RemoteFailure::RefGone {
                    refname: "fork".into(),
                    fix: RefGoneFix::ByHand,
                },
            ),
            (
                format!(
                    "ssh: Could not resolve hostname nonexistent.invalid: Name or service not \
                     known\n{COULD_NOT_READ}"
                ),
                unreachable(
                    UnreachableCause::Dns,
                    "ssh: Could not resolve hostname nonexistent.invalid: Name or service not \
                     known",
                ),
            ),
            (
                format!(
                    "ssh: connect to host 127.0.0.1 port 1: Connection refused\n{COULD_NOT_READ}"
                ),
                unreachable(
                    UnreachableCause::Connection,
                    "ssh: connect to host 127.0.0.1 port 1: Connection refused",
                ),
            ),
            (
                "ssh: connect to host 10.255.255.1 port 22: Connection timed out\n".into(),
                unreachable(
                    UnreachableCause::Connection,
                    "ssh: connect to host 10.255.255.1 port 22: Connection timed out",
                ),
            ),
            (
                format!(
                    "ssh: connect to host 2001:db8::1 port 22: Network is unreachable\n\
                     {COULD_NOT_READ}"
                ),
                unreachable(
                    UnreachableCause::Connection,
                    "ssh: connect to host 2001:db8::1 port 22: Network is unreachable",
                ),
            ),
            (
                "No ED25519 host key is known for github.com and you have requested strict \
                 checking.\nHost key verification failed.\n"
                    .into(),
                unreachable(
                    UnreachableCause::HostKey,
                    "No ED25519 host key is known for github.com and you have requested strict \
                     checking.",
                ),
            ),
            (
                format!("{HOST_KEY_CHANGED}{COULD_NOT_READ}"),
                unreachable(
                    UnreachableCause::HostKey,
                    "Host key for github.com has changed and you have requested strict checking.",
                ),
            ),
            (
                format!(
                    "Load key \"/dev/null\": error in libcrypto\n\
                     git@github.com: Permission denied (publickey).\n{COULD_NOT_READ}"
                ),
                unreachable(
                    UnreachableCause::Auth,
                    "git@github.com: Permission denied (publickey).",
                ),
            ),
            (
                format!("ERROR: Repository not found.\n{COULD_NOT_READ}"),
                RemoteFailure::RepoNotFound {
                    message: "ERROR: Repository not found.".into(),
                },
            ),
            (
                format!(
                    "fatal: '/srv/remotes/nope.git' does not appear to be a git repository\n\
                     {COULD_NOT_READ}"
                ),
                RemoteFailure::RepoNotFound {
                    message: "fatal: '/srv/remotes/nope.git' does not appear to be a git \
                              repository"
                        .into(),
                },
            ),
            // the visibility check's shapes, over HTTPS
            (
                "error: unable to read askpass response from '/bin/false'\n\
                 fatal: could not read Username for 'https://github.com': terminal prompts \
                 disabled\n"
                    .into(),
                unreachable(
                    UnreachableCause::Auth,
                    "fatal: could not read Username for 'https://github.com': terminal prompts \
                     disabled",
                ),
            ),
            (
                "fatal: Authentication failed for 'http://127.0.0.1:18401/me/app/'\n".into(),
                unreachable(
                    UnreachableCause::Auth,
                    "fatal: Authentication failed for 'http://127.0.0.1:18401/me/app/'",
                ),
            ),
            (
                "fatal: repository 'http://127.0.0.1:18404/me/app/' not found\n".into(),
                RemoteFailure::RepoNotFound {
                    message: "fatal: repository 'http://127.0.0.1:18404/me/app/' not found".into(),
                },
            ),
            (
                "fatal: unable to access 'https://nonexistent.invalid/me/app/': Could not resolve \
                 host: nonexistent.invalid\n"
                    .into(),
                unreachable(
                    UnreachableCause::Dns,
                    "fatal: unable to access 'https://nonexistent.invalid/me/app/': Could not \
                     resolve host: nonexistent.invalid",
                ),
            ),
            (
                "fatal: unable to access 'https://127.0.0.1:1/me/app/': Failed to connect to \
                 127.0.0.1 port 1 after 0 ms: Could not connect to server\n"
                    .into(),
                unreachable(
                    UnreachableCause::Connection,
                    "fatal: unable to access 'https://127.0.0.1:1/me/app/': Failed to connect to \
                     127.0.0.1 port 1 after 0 ms: Could not connect to server",
                ),
            ),
            (
                "fatal: unable to access 'https://127.0.0.1:18443/me/app/': server verification \
                 failed: certificate signer not trusted. (CAfile: \
                 /etc/ssl/certs/ca-certificates.crt CRLfile: none)\n"
                    .into(),
                unreachable(
                    UnreachableCause::HostKey,
                    "fatal: unable to access 'https://127.0.0.1:18443/me/app/': server \
                     verification failed: certificate signer not trusted. (CAfile: \
                     /etc/ssl/certs/ca-certificates.crt CRLfile: none)",
                ),
            ),
            // a 403 (a rate limit, say) says nothing about access
            (
                "fatal: unable to access 'http://127.0.0.1:18403/me/app/': The requested URL \
                 returned error: 403\n"
                    .into(),
                RemoteFailure::Failed {
                    message: "fatal: unable to access 'http://127.0.0.1:18403/me/app/': The \
                              requested URL returned error: 403"
                        .into(),
                },
            ),
            // the local refusal a protocol allowlist makes
            (
                "fatal: transport 'https' not allowed\n".into(),
                RemoteFailure::Failed {
                    message: "fatal: transport 'https' not allowed".into(),
                },
            ),
            (
                format!("\nfatal: the remote end hung up unexpectedly\n{COULD_NOT_READ}"),
                RemoteFailure::Failed {
                    message: "fatal: the remote end hung up unexpectedly".into(),
                },
            ),
        ];
        for (stderr, want) in cases {
            assert_eq!(
                RemoteFailure::from_stderr(&stderr, RefspecContext::default()),
                want,
                "{stderr}"
            );
        }
    }

    /// ssh's man-in-the-middle warning, captured with a `known_hosts` naming
    /// another key for the host (paths shortened).
    const HOST_KEY_CHANGED: &str = "\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!
Someone could be eavesdropping on you right now (man-in-the-middle attack)!
It is also possible that a host key has just been changed.
The fingerprint for the ED25519 key sent by the remote host is
SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.
Please contact your system administrator.
Add correct host key in /home/me/. to get rid of this message.
Offending ED25519 key in /home/me/fake_known_hosts:1
  remove with:
  ssh-keygen -f '/home/me/fake_known_hosts' -R 'github.com'
Host key for github.com has changed and you have requested strict checking.
Host key verification failed.
";

    #[test]
    fn classifies_runner_errors() {
        let failed = |stderr: &str, code| GitError::Failed {
            args: "fetch --prune --quiet origin".into(),
            code,
            stderr: stderr.into(),
        };
        assert_eq!(
            RemoteFailure::from_git_error(
                failed("fatal: couldn't find remote ref main", Some(128)),
                RefspecContext {
                    refspecs: &[ConfigValue::repo("main")],
                    branch: Some("dev"),
                }
            ),
            RemoteFailure::RefGone {
                refname: "main".into(),
                fix: RefGoneFix::SetBranches {
                    branch: Some("dev".into())
                },
            }
        );
        assert_eq!(
            RemoteFailure::from_git_error(
                GitError::Timeout {
                    args: "fetch".into(),
                    after: Duration::from_secs(120),
                },
                RefspecContext::default()
            ),
            RemoteFailure::TimedOut { after_secs: 120 }
        );
        assert_eq!(
            RemoteFailure::from_git_error(failed(" \n", Some(128)), RefspecContext::default()),
            RemoteFailure::Failed {
                message: "git exited 128 with no message".into()
            }
        );
        assert_eq!(
            RemoteFailure::from_git_error(failed("", None), RefspecContext::default()),
            RemoteFailure::Failed {
                message: "git was killed by a signal, with no message".into()
            }
        );
        assert_eq!(
            RemoteFailure::from_git_error(GitError::NotFound, RefspecContext::default()),
            RemoteFailure::Failed {
                message: "git not found on PATH".into()
            }
        );
    }

    #[test]
    fn failed_prefers_the_fatal_line() {
        let stderr = "warning: redirecting to https://github.com/me/app.git/\n\
                      12:00:00.000000 git.c:463 trace: built-in: git fetch\n\
                      fatal: the remote end hung up unexpectedly\n";
        assert_eq!(
            RemoteFailure::from_stderr(stderr, RefspecContext::default()),
            RemoteFailure::Failed {
                message: "fatal: the remote end hung up unexpectedly".into()
            }
        );
        // no `fatal:` line: the first non-empty one
        assert_eq!(
            RemoteFailure::from_stderr("\nerror: something odd\nmore\n", RefspecContext::default()),
            RemoteFailure::Failed {
                message: "error: something odd".into()
            }
        );
    }

    #[test]
    fn a_gone_ref_is_repaired_without_touching_other_refspecs() {
        let decide = |refname: &str, refspecs: &[ConfigValue], branch: Option<&str>| {
            RefGoneFix::decide(refname, RefspecContext { refspecs, branch })
        };
        let repo = ConfigValue::repo;
        let elsewhere = ConfigValue::elsewhere;
        let unset = |pattern: &str| RefGoneFix::UnsetRefspec {
            pattern: pattern.into(),
        };
        let set_branches = |b: Option<&str>| RefGoneFix::SetBranches {
            branch: b.map(str::to_owned),
        };
        // one line of several (a narrowed list), forced or not
        assert_eq!(
            decide(
                "refs/heads/feat",
                &[
                    repo("+refs/heads/main:refs/remotes/origin/main"),
                    repo("+refs/heads/feat:refs/remotes/origin/feat"),
                    repo("refs/heads/fork:refs/remotes/origin/fork"),
                ],
                Some("main")
            ),
            unset(r"^\+?refs/heads/feat(:|$)")
        );
        // a short refspec, regex characters escaped
        assert_eq!(
            decide(
                "feat.v2+x",
                &[
                    repo("+refs/heads/*:refs/remotes/origin/*"),
                    repo("feat.v2+x")
                ],
                None
            ),
            unset(r"^\+?feat\.v2\+x(:|$)")
        );
        // a prefix of another ref's name doesn't count
        assert_eq!(
            decide(
                "refs/heads/feat",
                &[repo("+refs/heads/feature:refs/remotes/origin/feature")],
                None
            ),
            RefGoneFix::ByHand
        );
        // the only refspec, or every one (duplicates): never leave none; the
        // registry's branch named, unless it's the one gone
        let solo = [repo("+refs/heads/solo:refs/remotes/origin/solo")];
        assert_eq!(
            decide("refs/heads/solo", &solo, Some("main")),
            set_branches(Some("main"))
        );
        assert_eq!(
            decide("refs/heads/solo", &solo, Some("solo")),
            set_branches(None)
        );
        assert_eq!(decide("refs/heads/solo", &solo, None), set_branches(None));
        assert_eq!(
            decide(
                "refs/heads/dup",
                &[
                    repo("+refs/heads/dup:refs/remotes/origin/dup"),
                    repo("refs/heads/dup:refs/remotes/origin/dup"),
                ],
                Some("main")
            ),
            set_branches(Some("main"))
        );
        // only negative refspecs, or another scope's, would remain
        assert_eq!(
            decide(
                "refs/heads/solo",
                &[
                    repo("+refs/heads/solo:refs/remotes/origin/solo"),
                    repo("^refs/heads/wip")
                ],
                Some("main")
            ),
            set_branches(Some("main"))
        );
        assert_eq!(
            decide(
                "refs/heads/solo",
                &[
                    elsewhere("+refs/pull/*/head:refs/remotes/origin/pr/*"),
                    repo("+refs/heads/solo:refs/remotes/origin/solo"),
                ],
                Some("main")
            ),
            set_branches(Some("main"))
        );
        // a refspec naming it outside the repo's own file: out of reach
        assert_eq!(
            decide(
                "refs/heads/feat",
                &[
                    repo("+refs/heads/main:refs/remotes/origin/main"),
                    elsewhere("+refs/heads/feat:refs/remotes/origin/feat"),
                ],
                Some("main")
            ),
            RefGoneFix::ByHand
        );
        assert_eq!(decide("main", &[], None), RefGoneFix::ByHand);
    }

    #[test]
    fn a_refused_anonymous_read_is_private_only_when_the_host_refused() {
        let url = "https://github.com/fuzdev/does-not-exist-xyz";
        let read = |stderr: &str| {
            VisibilityCheck::from_read(
                url,
                Err(GitError::Failed {
                    args: "ls-remote".into(),
                    code: Some(128),
                    stderr: stderr.into(),
                }),
            )
        };
        assert_eq!(
            VisibilityCheck::from_read(url, Ok(())),
            VisibilityCheck::Leak
        );
        // a local repo: its path quoted, not a lookalike's
        let local = |stderr: &str| {
            VisibilityCheck::from_read(
                "file:///srv/anon/me/app",
                Err(GitError::Failed {
                    args: "ls-remote".into(),
                    code: Some(128),
                    stderr: stderr.into(),
                }),
            )
        };
        assert_eq!(
            local("fatal: '/srv/anon/me/app' does not appear to be a git repository"),
            VisibilityCheck::Private
        );
        assert!(matches!(
            local("fatal: '/srv/anon/me/app2' does not appear to be a git repository"),
            VisibilityCheck::Unknown { .. }
        ));
        // the host's own refusals, as captured
        for stderr in [
            "error: unable to read askpass response from '/bin/false'\n\
             fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "fatal: Authentication failed for 'https://github.com/fuzdev/does-not-exist-xyz/'",
            "fatal: repository 'https://github.com/fuzdev/does-not-exist-xyz/' not found",
            "remote: Repository not found.\n\
             fatal: repository 'https://github.com/fuzdev/does-not-exist-xyz/' not found",
        ] {
            assert_eq!(read(stderr), VisibilityCheck::Private, "{stderr}");
        }
        // a refusal from anyone else: a proxy wanting its password (captured
        // through a local proxy), a lookalike host, a client certificate's
        // passphrase, a refusal naming no origin
        for stderr in [
            "error: unable to read askpass response from '/bin/false'\n\
             fatal: could not read Password for 'http://user@127.0.0.1:18777': terminal prompts \
             disabled\nfatal: remote helper 'https' aborted session",
            "fatal: could not read Username for 'https://github.com.evil': terminal prompts \
             disabled",
            "fatal: could not read Username for 'https://github.community': terminal prompts \
             disabled",
            "fatal: could not read Password for 'cert:///home/me/client.pem': terminal prompts \
             disabled",
            "remote: Repository not found.",
            "ERROR: Repository not found.",
        ] {
            assert!(
                matches!(read(stderr), VisibilityCheck::Unknown { .. }),
                "{stderr}"
            );
        }
        // the host named, but not refusing
        assert!(matches!(
            read(
                "fatal: unable to access 'https://github.com/fuzdev/does-not-exist-xyz/': \
                 CONNECT tunnel failed, response 407"
            ),
            VisibilityCheck::Unknown {
                failure: RemoteFailure::Failed { .. }
            }
        ));
        for e in [
            GitError::Timeout {
                args: "ls-remote".into(),
                after: Duration::from_secs(120),
            },
            GitError::NotFound,
        ] {
            assert!(matches!(
                VisibilityCheck::from_read(url, Err(e)),
                VisibilityCheck::Unknown { .. }
            ));
        }
        // the proxy's refusal is still classified, for the report
        assert!(matches!(
            read("fatal: could not read Password for 'http://user@127.0.0.1:18777': x"),
            VisibilityCheck::Unknown {
                failure: RemoteFailure::Unreachable {
                    cause: UnreachableCause::Auth,
                    ..
                }
            }
        ));
    }
}
