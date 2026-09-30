//! `FixtureWorkspace`: a tempdir workspace of real git repos for the
//! integration tests — a bare "remote" per repo, an upstream author's clone
//! that pushes to it, a clone per registry entry, and a generated
//! `repos.toml`.
//!
//! Every git call is hermetic: the environment is cleared down to `PATH`, a
//! throwaway `HOME`, no global or system config, fixed identities, and a
//! fixed clock that advances a minute per call, so commit times are
//! deterministic. Remotes are `file://` URLs, so `--depth` and `--filter`
//! apply and nothing reaches the network. The library runs under the same
//! environment through `Git::with_clean_env`, and the binary through
//! `FixtureWorkspace::command`.
//!
//! A clone's `origin` holds the URL the registry expects (SSH for owned
//! entries, HTTPS for third-party ones) — the probe reads it raw from config
//! — and a repo-local `url.<file URL>.insteadOf` sends fetches to the local
//! bare remote. A push goes where it would for real, the registry's URL (an
//! identity `pushInsteadOf` exempts it from the fetch's rewrite): over SSH,
//! where `ssh` on `PATH` is the fixture's own (`FIXTURE_SSH`), which serves
//! the owner's repos from the local bare remotes and refuses anything else,
//! so no push ever leaves the tempdir. `GIT_ALLOW_PROTOCOL=file:ssh` makes
//! any other transport an error (unless a test widens it,
//! `allow_transport`). A call that sets its own allowlist — a third-party
//! clone allows `https` alone — still reaches no host: `GIT_EXEC_PATH` is
//! the fixture's, git's own programs linked but the curl helpers for
//! `https`, `ftp`, and `ftps`, and its `git-remote-https` refuses every URL
//! (`https_refused_log`) until a test swaps in one serving the bare
//! remotes (`serve_https`). The visibility check reads under
//! `visibility_base`, a `file://` dir or a loopback `http` server, so it
//! stays local too.
//!
//! Setups assert the git state they build (`assert_track` and kin) before the
//! tool reads it: a setup that silently builds the wrong state tests nothing.
//! `snapshot_git_dir` and `assert_git_dir_unchanged` pin that a tool call
//! wrote nothing to a git dir.
//!
//! Busy detection reads Claude Code's config dir: the binary finds none
//! under the throwaway `HOME`, and the library runs are handed no live
//! session, unless a test builds a `ClaudeDir` — session files naming real
//! live pids, its own `LiveChild` processes — and points the tool at it.

// each test binary uses a subset of the helpers
#![allow(dead_code)]
// test support: a setup that can't build its fixture fails the test, as an
// assertion would
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use fuz_repos::classify::Refresh;
use fuz_repos::clone::CLONE_TIMEOUT;
use fuz_repos::discover::{REGISTRY_FILE, find_registry, resolve_push_targets};
use fuz_repos::git::Git;
use fuz_repos::probe::RegistryDirs;
use fuz_repos::push::{PushOptions, PushRun, check_pushable, push};
use fuz_repos::registry::{Entry, ValidRegistry};
use fuz_repos::report::{EntryStatus, UnregisteredClone};
use fuz_repos::scan::scan_unregistered;
use fuz_repos::sessions::{Caller, LiveSessions, stat_starttime};
use fuz_repos::state::{BranchStatus, UnprobedWorktree};
use fuz_repos::status::{StatusOptions, StatusRun, status};
use fuz_repos::sync::{SyncOptions, SyncRun, sync};
use tempfile::TempDir;

/// The registry's owner account: its repos are writable.
pub const OWNER: &str = "me";
/// A third-party account: its repos are read-only references.
pub const THIRD_PARTY: &str = "them";
/// The fixture clock's start, in unix seconds.
pub const CLOCK_START: u64 = 1_700_000_000;
/// How far the clock moves per git call.
const TICK: u64 = 60;

/// The fixture's `ssh`, first on `PATH`: serves
/// `git@github.com:<OWNER>/<name>` from the local bare remote `<name>.git`
/// under `@REMOTES@` — as a real host would, without the client's
/// `GIT_CONFIG_PARAMETERS` (the runner's hardening), so the bare remote's
/// own hooks run — and refuses any other host, command, or path. A remote
/// holding a `fixture-stall` file answers only after five seconds
/// (`stall_remote`). Each call's arguments are appended to `@LOG@`.
const FIXTURE_SSH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> '@LOG@'
host=; cmd=
for arg; do host=$cmd; cmd=$arg; done
if [ "$host" != git@github.com ]; then
	echo "fixture ssh: refused host $host" >&2; exit 255
fi
case $cmd in
"git-receive-pack '"*"'") verb=receive-pack ;;
"git-upload-pack '"*"'") verb=upload-pack ;;
*) echo "fixture ssh: refused command $cmd" >&2; exit 255 ;;
esac
path=${cmd#* \'}; path=${path%\'}; path=${path#/}; path=${path%.git}
name=${path#@OWNER@/}
case $name in
''|*/*|"$path") echo "fixture ssh: refused path $path" >&2; exit 255 ;;
esac
if [ ! -d '@REMOTES@'/"$name.git" ]; then
	echo "ERROR: Repository not found." >&2; exit 1
fi
if [ -e '@REMOTES@'/"$name.git/fixture-stall" ]; then sleep 5; fi
unset GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE \
	GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE
exec git "$verb" '@REMOTES@'/"$name.git"
"#;

/// The fixture's `git-remote-https` by default: refuses every URL, so no
/// test reaches the network over HTTPS, whatever `GIT_ALLOW_PROTOCOL` a
/// call sets — the clone of a third-party entry allows `https` alone,
/// replacing the fixture's `file:ssh`. Each URL it refuses is appended to
/// `@LOG@`. `serve_https` swaps in `FIXTURE_HTTPS`.
const FIXTURE_HTTPS_REFUSED: &str = r#"#!/bin/sh
printf '%s\n' "$2" >> '@LOG@'
echo "fixture https: refused $2 (no network in tests; serve_https serves the remotes)" >&2
exit 128
"#;

/// The fixture's `git-remote-https` once `serve_https` swaps it in: serves
/// `https://github.com/<THIRD_PARTY>/<name>` from the local bare remote
/// `<name>.git` under `@REMOTES@` over the remote-helper protocol's
/// `connect` (protocol v2, as a real host speaks it, without the client's
/// `GIT_CONFIG_PARAMETERS` or repo variables), answers a repo it doesn't
/// have as GitHub does, and refuses any other URL. Each call's URL is
/// appended to `@LOG@`.
const FIXTURE_HTTPS: &str = r#"#!/bin/sh
printf '%s\n' "$2" >> '@LOG@'
case $2 in
https://github.com/@THIRD_PARTY@/*) name=${2#https://github.com/@THIRD_PARTY@/} ;;
*) echo "fixture https: refused $2" >&2; exit 128 ;;
esac
name=${name%.git}
case $name in
''|*/*) echo "fixture https: refused $2" >&2; exit 128 ;;
esac
if [ ! -d '@REMOTES@'/"$name.git" ]; then
	echo "remote: Repository not found." >&2
	echo "fatal: repository '$2/' not found" >&2
	exit 128
fi
while read -r line; do
	case $line in
	capabilities) printf 'connect\n\n' ;;
	'connect git-upload-pack')
		printf '\n'
		unset GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE \
			GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE
		GIT_PROTOCOL=version=2 exec git upload-pack '@REMOTES@'/"$name.git" ;;
	'') exit 0 ;;
	*) echo "fixture https: unexpected $line" >&2; exit 128 ;;
	esac
done
"#;

/// The programs in git's own exec path the fixture's never links: the
/// curl helpers for the transports that would reach a real host (`http`
/// stays, for the loopback server the visibility tests read).
const UNLINKED_EXEC: [&str; 3] = ["git-remote-https", "git-remote-ftp", "git-remote-ftps"];

/// Git's own exec path, as the environment the tests run in has it.
fn real_exec_path() -> &'static Path {
    static EXEC_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EXEC_PATH.get_or_init(|| {
        let out = Command::new("git")
            .arg("--exec-path")
            .env_remove("GIT_EXEC_PATH")
            .output()
            .unwrap();
        assert!(out.status.success(), "git --exec-path failed");
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
    })
}

/// How `FixtureWorkspace::sync_full` runs `sync`.
pub struct SyncRunOptions<'a> {
    pub caller: Caller,
    pub jobs: usize,
    pub read_live: &'a (dyn Fn() -> LiveSessions + Sync),
    pub clone_timeout: Duration,
    pub refresh: Refresh,
    /// Run the unregistered scan first and hand sync its dirs, as a run
    /// without targets does.
    pub scanned: bool,
}

/// A workspace of fixture repos under one tempdir.
#[derive(Debug)]
pub struct FixtureWorkspace {
    /// Held for its drop, which deletes the tree.
    _tmp: TempDir,
    /// The tempdir's canonical path: a symlinked `TMPDIR` (macOS) would
    /// otherwise make git's resolved paths differ from the fixture's.
    base: PathBuf,
    clock: Cell<u64>,
    /// The registry's tables, in declaration order.
    tables: Vec<String>,
    /// `GIT_ALLOW_PROTOCOL`: `file:ssh`, unless a test widens it.
    allowed_protocols: String,
    /// More variables every call sees (`set_env`).
    extra_env: Vec<(OsString, OsString)>,
}

impl Default for FixtureWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureWorkspace {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for dir in ["ws", "remotes", "upstream", "home", "ssh-bin"] {
            std::fs::create_dir(base.join(dir)).unwrap();
        }
        let ssh = FIXTURE_SSH
            .replace("@LOG@", base.join("ssh.log").to_str().unwrap())
            .replace("@REMOTES@", base.join("remotes").to_str().unwrap())
            .replace("@OWNER@", OWNER);
        write_executable(&base.join("ssh-bin"), "ssh", &ssh);
        // git's exec path, every program linked but the helpers that would
        // reach a real host, and the refusing `git-remote-https`
        let exec = base.join("git-exec");
        std::fs::create_dir(&exec).unwrap();
        for program in std::fs::read_dir(real_exec_path()).unwrap() {
            let name = program.unwrap().file_name();
            if !UNLINKED_EXEC.iter().any(|u| name == *u) {
                std::os::unix::fs::symlink(real_exec_path().join(&name), exec.join(&name)).unwrap();
            }
        }
        let refused = FIXTURE_HTTPS_REFUSED
            .replace("@LOG@", base.join("https-refused.log").to_str().unwrap());
        write_executable(&exec, "git-remote-https", &refused);
        Self {
            _tmp: tmp,
            base,
            clock: Cell::new(CLOCK_START),
            tables: Vec::new(),
            allowed_protocols: "file:ssh".into(),
            extra_env: Vec::new(),
        }
    }

    /// The workspace root, where the registry and the clones live.
    pub fn root(&self) -> PathBuf {
        self.base.as_path().join("ws")
    }

    /// The tempdir holding the workspace, the remotes, and the upstream
    /// clones.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// A dir under the tempdir but outside the workspace.
    pub fn outside(&self, name: &str) -> PathBuf {
        self.base.as_path().join(name)
    }

    /// An entry dir under the workspace root.
    pub fn dir(&self, dir: &str) -> PathBuf {
        self.root().join(dir)
    }

    /// The bare remote for a repo name.
    pub fn bare(&self, name: &str) -> PathBuf {
        self.base
            .as_path()
            .join("remotes")
            .join(format!("{name}.git"))
    }

    /// The upstream author's clone of a repo name.
    pub fn upstream(&self, name: &str) -> PathBuf {
        self.base.as_path().join("upstream").join(name)
    }

    fn file_url(&self, name: &str) -> String {
        format!("file://{}", self.bare(name).display())
    }

    // --- the environment ---

    /// The only environment fixture git calls, the library, and the binary
    /// see (minus the clock, which `command` adds).
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let home = self.base.as_path().join("home");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), home.clone().into()),
            ("XDG_CONFIG_HOME".into(), home.into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_AUTHOR_NAME".into(), "Fixture Author".into()),
            ("GIT_AUTHOR_EMAIL".into(), "author@example.com".into()),
            ("GIT_COMMITTER_NAME".into(), "Fixture Committer".into()),
            ("GIT_COMMITTER_EMAIL".into(), "committer@example.com".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
            // nothing but the local bare remotes, ever: `ssh` is the
            // fixture's own, which serves them
            (
                "GIT_ALLOW_PROTOCOL".into(),
                (&self.allowed_protocols).into(),
            ),
        ];
        // the fixture's `ssh` before any other
        let path = std::env::join_paths(std::iter::once(self.base.join("ssh-bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        env.push(("PATH".into(), path));
        // git's own programs, `git-remote-https` the fixture's
        env.push(("GIT_EXEC_PATH".into(), self.base.join("git-exec").into()));
        env.extend(self.extra_env.iter().cloned());
        env
    }

    /// Serves third-party repos over the fixture's `https` for every later
    /// call (`FIXTURE_HTTPS`), from the bare remotes, in place of the one
    /// that refuses every URL; each URL it's asked for is on `https_log`.
    pub fn serve_https(&self) {
        let helper = FIXTURE_HTTPS
            .replace("@LOG@", self.base.join("https.log").to_str().unwrap())
            .replace("@REMOTES@", self.base.join("remotes").to_str().unwrap())
            .replace("@THIRD_PARTY@", THIRD_PARTY);
        write_executable(&self.base.join("git-exec"), "git-remote-https", &helper);
    }

    /// Every URL the fixture's serving `https` was asked for.
    pub fn https_log(&self) -> Vec<String> {
        read_lines(&self.base.join("https.log"))
    }

    /// Every URL the fixture's refusing `https` refused: asked for without
    /// `serve_https`.
    pub fn https_refused_log(&self) -> Vec<String> {
        read_lines(&self.base.join("https-refused.log"))
    }

    /// Makes `name`'s bare remote answer the fixture's `ssh` only after five
    /// seconds.
    pub fn stall_remote(&self, name: &str) {
        std::fs::write(self.bare(name).join("fixture-stall"), "").unwrap();
    }

    /// Sets `name` for every later call, the library's included.
    pub fn set_env(&mut self, name: &str, value: impl Into<OsString>) {
        self.extra_env.push((name.into(), value.into()));
    }

    /// Widens `GIT_ALLOW_PROTOCOL` by `transport` for every later call, the
    /// library's included: a test of a call that allows less than the
    /// caller's environment.
    pub fn allow_transport(&mut self, transport: &str) {
        self.allowed_protocols = format!("{}:{transport}", self.allowed_protocols);
    }

    /// A dir on every call's `PATH`, first: where a test puts a program
    /// git looks up there, like a remote helper (`git-remote-<name>`).
    pub fn bin(&self) -> PathBuf {
        self.base.join("ssh-bin")
    }

    /// Every call the fixture's `ssh` served or refused, its arguments one
    /// line each.
    pub fn ssh_log(&self) -> Vec<String> {
        read_lines(&self.base.join("ssh.log"))
    }

    /// The library's runner, under the hermetic environment.
    pub fn runner(&self) -> Git {
        Git::with_clean_env(self.env())
    }

    /// Advances the clock and returns the new time.
    fn tick(&self) -> u64 {
        let t = self.clock.get() + TICK;
        self.clock.set(t);
        t
    }

    /// A command under the hermetic environment, with the clock's next time
    /// as the author and committer date.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
        let date = format!("@{} +0000", self.tick());
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .envs(self.env())
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(cwd)
            .stdin(Stdio::null());
        cmd
    }

    /// Runs git in `cwd` and returns its output whatever the exit status.
    pub fn git_output(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("git", cwd).args(args).output().unwrap()
    }

    /// Runs git in `cwd`, asserting success; returns trimmed stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_raw(cwd, args).trim().to_owned()
    }

    /// Runs git in `cwd`, asserting success; returns stdout as is.
    pub fn git_raw(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git_output(cwd, args);
        assert!(
            out.status.success(),
            "git {} in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs git in `cwd`, asserting it fails (a stopped rebase, a conflict).
    pub fn git_fails(&self, cwd: &Path, args: &[&str]) {
        let out = self.git_output(cwd, args);
        assert!(
            !out.status.success(),
            "git {} in {} unexpectedly succeeded",
            args.join(" "),
            cwd.display()
        );
    }

    // --- remotes and upstream history ---

    /// Creates the bare remote for `name` with one commit on `main` (a
    /// `README` plus `files`), pushed from the upstream author's clone.
    pub fn remote(&self, name: &str, files: &[(&str, &str)]) {
        let bare = self.bare(name);
        let bare_s = bare.to_str().unwrap();
        self.git(
            self.base.as_path(),
            &["-c", "init.defaultBranch=main", "init", "--bare", bare_s],
        );
        // serve `--filter` for partial clones
        self.git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let up = self.upstream(name);
        let up_s = up.to_str().unwrap();
        self.git(
            self.base.as_path(),
            &["-c", "init.defaultBranch=main", "init", up_s],
        );
        self.git(&up, &["remote", "add", "origin", &self.file_url(name)]);
        write(&up, "README", &format!("# {name}\n"));
        for (path, content) in files {
            write(&up, path, content);
        }
        self.git(&up, &["add", "-A"]);
        self.git(&up, &["commit", "-q", "-m", "initial"]);
        self.git(&up, &["push", "-q", "-u", "origin", "main"]);
    }

    /// Commits a change on `branch` upstream and pushes it; the branch is
    /// created from `main` when it doesn't exist. Returns the commit.
    pub fn upstream_commit(&self, name: &str, branch: &str) -> String {
        let up = self.upstream(name);
        if self.has_ref(&up, branch) {
            self.git(&up, &["checkout", "-q", branch]);
        } else {
            self.git(&up, &["checkout", "-q", "-b", branch, "main"]);
        }
        let oid = self.commit(&up, &format!("upstream-{branch}"));
        self.git(&up, &["push", "-q", "origin", branch]);
        self.git(&up, &["checkout", "-q", "main"]);
        oid
    }

    /// Deletes `branch` on the remote.
    pub fn upstream_delete_branch(&self, name: &str, branch: &str) {
        self.git(
            &self.upstream(name),
            &["push", "-q", "origin", "--delete", branch],
        );
    }

    // --- clones ---

    /// Clones `name` into the workspace as an owned entry would be: `origin`
    /// is the registry's SSH URL, fetched through the local bare remote.
    pub fn clone_owned(&self, dir: &str, name: &str, args: &[&str]) -> PathBuf {
        self.clone_as(dir, name, &owned_origin(name), args)
    }

    /// An owned repo in one step: its remote (a `README` plus `files`), its
    /// `[repos.<name>]` entry, and its clone at `<root>/<name>`.
    pub fn owned_repo(&mut self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        self.remote(name, files);
        self.declare_repo(name, name, "");
        self.clone_owned(name, name, &[])
    }

    /// Clones `name` as a third-party reference: `origin` is its HTTPS URL.
    pub fn clone_third_party(&self, dir: &str, name: &str, args: &[&str]) -> PathBuf {
        self.clone_as(dir, name, &third_party_origin(name), args)
    }

    /// Clones `name` as a third-party reference whose fetches go where
    /// they would for real, its HTTPS URL, with nothing rewriting them:
    /// the fixture's `https` serves them once `serve_https` swaps it in,
    /// and refuses them until then, logging each.
    pub fn clone_third_party_over_https(&self, dir: &str, name: &str, args: &[&str]) -> PathBuf {
        let dest = self.dir(dir);
        let url = self.file_url(name);
        let mut clone = vec!["clone", "-q"];
        clone.extend(args);
        clone.extend([url.as_str(), dest.to_str().unwrap()]);
        self.git(&self.root(), &clone);
        let origin = third_party_origin(name);
        self.git(&dest, &["remote", "set-url", "origin", &origin]);
        assert_eq!(
            self.git(&dest, &["ls-remote", "--get-url", "origin"]),
            origin
        );
        dest
    }

    /// Clones `name` into `<root>/<dir>` with `args`, sets `origin` to
    /// `origin`, and routes fetches for it to the bare remote.
    pub fn clone_as(&self, dir: &str, name: &str, origin: &str, args: &[&str]) -> PathBuf {
        let dest = self.dir(dir);
        let url = self.file_url(name);
        let mut clone = vec!["clone", "-q"];
        clone.extend(args);
        clone.extend([url.as_str(), dest.to_str().unwrap()]);
        self.git(&self.root(), &clone);
        self.set_origin(&dest, name, origin);
        dest
    }

    /// Points `origin` at `url` while fetches still reach `name`'s bare
    /// remote.
    pub fn set_origin(&self, repo: &Path, name: &str, url: &str) {
        self.git(repo, &["remote", "set-url", "origin", url]);
        let key = format!("url.{}.insteadOf", self.file_url(name));
        self.git(repo, &["config", &key, url]);
        // a push goes to `url` itself, where the fixture's `ssh` serves it
        let key = format!("url.{url}.pushInsteadOf");
        self.git(repo, &["config", &key, url]);
        assert_eq!(self.git(repo, &["config", "remote.origin.url"]), url);
        // what a fetch actually reaches, and a push
        assert_eq!(
            self.git(repo, &["ls-remote", "--get-url", "origin"]),
            self.file_url(name)
        );
        assert_eq!(
            self.git(repo, &["remote", "get-url", "--push", "--all", "origin"]),
            url
        );
    }

    /// Adds a linked worktree of `repo` at `path` (`args` follow it: a
    /// branch to check out, `-b <new>`, `--detach`, …) and returns its own
    /// git dir, `<commondir>/worktrees/<id>`.
    pub fn add_worktree(&self, repo: &Path, path: &Path, args: &[&str]) -> PathBuf {
        let mut all = vec!["worktree", "add", "-q", path.to_str().unwrap()];
        all.extend(args);
        self.git(repo, &all);
        assert!(path.join(".git").is_file(), "{} is linked", path.display());
        let admin = PathBuf::from(self.git(path, &["rev-parse", "--absolute-git-dir"]));
        let common = PathBuf::from(self.git(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        assert_eq!(admin.parent(), Some(common.join("worktrees").as_path()));
        admin
    }

    /// The `git worktree list --porcelain` record for the worktree at
    /// `path`, as its attribute lines.
    pub fn worktree_record(&self, repo: &Path, path: &Path) -> Vec<String> {
        let out = self.git_raw(repo, &["worktree", "list", "--porcelain"]);
        let head = format!("worktree {}", path.display());
        out.split("\n\n")
            .map(|r| r.lines().map(str::to_owned).collect::<Vec<_>>())
            .find(|r| r.first() == Some(&head))
            .unwrap_or_else(|| panic!("no worktree {} in:\n{out}", path.display()))
    }

    /// Writes a file and commits it; returns the commit.
    pub fn commit(&self, repo: &Path, label: &str) -> String {
        let n = self.clock.get();
        write(repo, &format!("{label}.txt"), &format!("{label} {n}\n"));
        self.git(repo, &["add", "-A"]);
        self.git(repo, &["commit", "-q", "-m", label]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    /// The committer time of `rev`, in unix seconds.
    pub fn committer_time(&self, repo: &Path, rev: &str) -> u64 {
        self.git(repo, &["log", "-1", "--format=%ct", rev])
            .parse()
            .unwrap()
    }

    // --- the registry ---

    /// Declares an owned public repo at `https://github.com/me/<name>`;
    /// `extra` is more TOML for its table (`dir`, `branch`, `archived`).
    pub fn declare_repo(&mut self, key: &str, name: &str, extra: &str) {
        self.declare_repo_as(key, name, "public", extra);
    }

    /// Declares a repo at `url` as written, with a `visibility`.
    pub fn declare_repo_url(&mut self, key: &str, url: &str, visibility: &str) {
        self.tables.push(format!(
            "[repos.{key}]\nurl = \"{url}\"\nvisibility = \"{visibility}\"\n\
             purpose = \"fixture\"\n"
        ));
    }

    /// Declares an owned repo with a `visibility` (`public`, `private`).
    pub fn declare_repo_as(&mut self, key: &str, name: &str, visibility: &str, extra: &str) {
        self.tables.push(format!(
            "[repos.{key}]\nurl = \"https://github.com/{OWNER}/{name}\"\n\
             visibility = \"{visibility}\"\npurpose = \"fixture\"\n{extra}\n"
        ));
    }

    /// Declares a reference at `https://github.com/<account>/<name>`; `extra`
    /// is more TOML (`branch`, `pinned`, `shallow`, `sparse`).
    pub fn declare_reference(&mut self, key: &str, account: &str, name: &str, extra: &str) {
        self.tables.push(format!(
            "[references.{key}]\nurl = \"https://github.com/{account}/{name}\"\n\
             purpose = \"fixture\"\n{extra}\n"
        ));
    }

    /// The generated registry document.
    pub fn registry_toml(&self) -> String {
        let mut out = format!("owners = [\"{OWNER}\"]\n\n");
        for table in &self.tables {
            let _ = writeln!(out, "{table}");
        }
        out
    }

    /// Writes the registry at the workspace root.
    pub fn write_registry(&self) -> PathBuf {
        let path = self.root().join(REGISTRY_FILE);
        std::fs::write(&path, self.registry_toml()).unwrap();
        path
    }

    /// Writes the registry in `dir` — one of the workspace's repos, say —
    /// and links it at the root by its absolute path; returns the file.
    pub fn write_registry_in(&self, dir: &Path) -> PathBuf {
        let path = dir.join(REGISTRY_FILE);
        write(dir, REGISTRY_FILE, &self.registry_toml());
        let link = self.root().join(REGISTRY_FILE);
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(link.canonicalize().unwrap(), path);
        path
    }

    /// Writes the registry and loads its entries the way the binary does:
    /// found by walking up from the root.
    pub fn entries(&self) -> Vec<Entry> {
        self.write_registry();
        let loc = find_registry(&self.root(), None, None, &self.runner()).unwrap();
        assert_eq!(loc.root, self.root());
        ValidRegistry::load(&loc.path).unwrap().entries()
    }

    // --- running the tool ---

    /// `status` over every entry, local refs only.
    pub fn status(&self) -> Vec<EntryStatus> {
        self.status_at(&self.root(), false)
    }

    /// `status --fetch` over every entry.
    pub fn status_with_fetch(&self) -> Vec<EntryStatus> {
        self.status_at(&self.root(), true)
    }

    /// `status` over every entry with `root` as the workspace root — another
    /// path to the same dir, such as a symlink to it.
    pub fn status_at(&self, root: &Path, fetch: bool) -> Vec<EntryStatus> {
        self.status_with(root, fetch, &self.runner(), &self.visibility_base())
    }

    /// `status` over every entry with `root` as the workspace root, run by
    /// `git`, the visibility check reading repos under `visibility_base`.
    pub fn status_with(
        &self,
        root: &Path,
        fetch: bool,
        git: &Git,
        visibility_base: &str,
    ) -> Vec<EntryStatus> {
        self.status_asked_with(root, fetch, git, visibility_base, Refresh::Unasked, None)
    }

    /// `status` over every entry, refreshing the references `refresh`
    /// asks for — `Refresh::Named` as a run naming every entry would.
    pub fn status_asked(&self, fetch: bool, refresh: Refresh) -> Vec<EntryStatus> {
        let root = self.root();
        self.status_asked_with(
            &root,
            fetch,
            &self.runner(),
            &self.visibility_base(),
            refresh,
            None,
        )
    }

    /// `status` over every entry, local refs only, after the unregistered
    /// scan, as a run without targets makes it.
    pub fn status_scanned(&self) -> Vec<EntryStatus> {
        let root = self.root();
        let unregistered = self.unregistered();
        self.status_asked_with(
            &root,
            false,
            &self.runner(),
            &self.visibility_base(),
            Refresh::Unasked,
            Some(&unregistered),
        )
    }

    /// `status_with`, refreshing what `refresh` asks for, with the
    /// unregistered scan's dirs when it ran.
    pub fn status_asked_with(
        &self,
        root: &Path,
        fetch: bool,
        git: &Git,
        visibility_base: &str,
        refresh: Refresh,
        unregistered: Option<&[UnregisteredClone]>,
    ) -> Vec<EntryStatus> {
        let entries = self.entries();
        let run = status(
            &entries,
            &RegistryDirs::new(root, &entries),
            root,
            git,
            StatusOptions {
                fetch,
                refresh,
                unregistered,
                jobs: 4,
                visibility_base: Some(visibility_base),
                live: &LiveSessions::Known(vec![]),
                caller: Caller::Person,
            },
        );
        run.entries
    }

    /// `status` over every entry, local refs only, with `live` as the live
    /// sessions the reader found.
    pub fn status_live(&self, live: &LiveSessions) -> StatusRun {
        self.status_live_at(&self.root(), live)
    }

    /// `status_live` with `root` as the workspace root.
    pub fn status_live_at(&self, root: &Path, live: &LiveSessions) -> StatusRun {
        let entries = self.entries();
        status(
            &entries,
            &RegistryDirs::new(root, &entries),
            root,
            &self.runner(),
            StatusOptions {
                fetch: false,
                refresh: Refresh::Unasked,
                unregistered: None,
                jobs: 4,
                visibility_base: Some(&self.visibility_base()),
                live,
                caller: Caller::Person,
            },
        )
    }

    /// `sync` over every entry with `jobs` in flight, `read_live` reading
    /// the live sessions each time sync asks, run by a person.
    pub fn sync_with(&self, jobs: usize, read_live: &(dyn Fn() -> LiveSessions + Sync)) -> SyncRun {
        self.sync_as(Caller::Person, jobs, read_live)
    }

    /// `sync_with`, run by `caller`.
    pub fn sync_as(
        &self,
        caller: Caller,
        jobs: usize,
        read_live: &(dyn Fn() -> LiveSessions + Sync),
    ) -> SyncRun {
        self.sync_timed(caller, jobs, read_live, CLONE_TIMEOUT)
    }

    /// `sync_as`, each clone under `clone_timeout`.
    pub fn sync_timed(
        &self,
        caller: Caller,
        jobs: usize,
        read_live: &(dyn Fn() -> LiveSessions + Sync),
        clone_timeout: Duration,
    ) -> SyncRun {
        self.sync_full(&SyncRunOptions {
            caller,
            jobs,
            read_live,
            clone_timeout,
            refresh: Refresh::Unasked,
            scanned: false,
        })
    }

    /// `sync` by a person, no live session anywhere, refreshing the
    /// references `refresh` asks for, with `jobs` in flight.
    pub fn sync_asked(&self, refresh: Refresh, jobs: usize) -> SyncRun {
        self.sync_full(&SyncRunOptions {
            caller: Caller::Person,
            jobs,
            read_live: &|| LiveSessions::Known(vec![]),
            clone_timeout: CLONE_TIMEOUT,
            refresh,
            scanned: false,
        })
    }

    /// `sync` by a person, no live session anywhere, after the unregistered
    /// scan, as a run without targets makes it.
    pub fn sync_scanned(&self) -> SyncRun {
        self.sync_full(&SyncRunOptions {
            caller: Caller::Person,
            jobs: 4,
            read_live: &|| LiveSessions::Known(vec![]),
            clone_timeout: CLONE_TIMEOUT,
            refresh: Refresh::Unasked,
            scanned: true,
        })
    }

    /// `sync` over every entry, as `opts` says.
    pub fn sync_full(&self, opts: &SyncRunOptions<'_>) -> SyncRun {
        let entries = self.entries();
        let root = self.root();
        let unregistered = opts.scanned.then(|| self.unregistered());
        sync(
            &entries,
            &RegistryDirs::new(&root, &entries),
            &root,
            &self.runner(),
            SyncOptions {
                jobs: opts.jobs,
                visibility_base: Some(&self.visibility_base()),
                read_live: &opts.read_live,
                caller: opts.caller,
                clone_timeout: opts.clone_timeout,
                refresh: opts.refresh,
                unregistered: unregistered.as_deref(),
            },
        )
    }

    /// `sync` over every entry, no live session anywhere.
    pub fn sync(&self) -> SyncRun {
        self.sync_with(4, &|| LiveSessions::Known(vec![]))
    }

    /// `repos push` of `targets`, resolved from `cwd` as the binary resolves
    /// them (none: the checkout holding `cwd`), `read_live` reading the live
    /// sessions each time the push asks.
    pub fn push_with(
        &self,
        targets: &[&str],
        cwd: &Path,
        read_live: &(dyn Fn() -> LiveSessions + Sync),
    ) -> PushRun {
        self.push_full(targets, cwd, read_live, false)
    }

    /// `repos push`, as `push_with`, creating a branch with no upstream on
    /// origin when `new_branch` (`--new-branch`).
    pub fn push_full(
        &self,
        targets: &[&str],
        cwd: &Path,
        read_live: &(dyn Fn() -> LiveSessions + Sync),
        new_branch: bool,
    ) -> PushRun {
        let entries = self.entries();
        let root = self.root();
        let git = self.runner();
        let targets: Vec<String> = targets.iter().map(|&t| t.to_owned()).collect();
        let resolved = resolve_push_targets(&entries, &root, cwd, &targets, &git).unwrap();
        check_pushable(&resolved).unwrap();
        push(
            &resolved,
            &RegistryDirs::new(&root, &entries),
            &root,
            &git,
            PushOptions {
                jobs: 4,
                visibility_base: Some(&self.visibility_base()),
                read_live,
                new_branch,
            },
        )
    }

    /// `repos push` of `targets` from the workspace root, no live session
    /// anywhere.
    pub fn push(&self, targets: &[&str]) -> PushRun {
        self.push_with(targets, &self.root(), &|| LiveSessions::Known(vec![]))
    }

    /// `repos push --new-branch` of `targets` from the workspace root, no
    /// live session anywhere.
    pub fn push_new_branch(&self, targets: &[&str]) -> PushRun {
        self.push_full(targets, &self.root(), &|| LiveSessions::Known(vec![]), true)
    }

    /// Every ref of `repo` (`refs/…` by name, with its object), and `HEAD`:
    /// the ref it names, or its commit when detached.
    pub fn refs(&self, repo: &Path) -> BTreeMap<String, String> {
        let mut refs: BTreeMap<String, String> = self
            .git_raw(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .map(|l| {
                let (name, oid) = l.split_once(' ').unwrap();
                (name.to_owned(), oid.to_owned())
            })
            .collect();
        let head = self.git_output(repo, &["symbolic-ref", "-q", "HEAD"]);
        let head = if head.status.success() {
            String::from_utf8(head.stdout).unwrap().trim().to_owned()
        } else {
            self.git(repo, &["rev-parse", "HEAD"])
        };
        refs.insert("HEAD".into(), head);
        refs
    }

    /// `refs` as they should read after a fetch of `name`'s bare remote:
    /// `before`, each `refs/remotes/origin/<b>` at the remote's `<b>` (its
    /// `HEAD` symref at the remote's HEAD branch), then `changes`.
    pub fn refs_after_fetch(
        &self,
        name: &str,
        before: &BTreeMap<String, String>,
        changes: &[(&str, &str)],
    ) -> BTreeMap<String, String> {
        let bare = self.bare(name);
        let mut refs = before.clone();
        refs.retain(|r, _| !r.starts_with("refs/remotes/origin/") || r.ends_with("/HEAD"));
        for l in self
            .git_raw(
                &bare,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ],
            )
            .lines()
        {
            let (name, oid) = l.split_once(' ').unwrap();
            let branch = name.strip_prefix("refs/heads/").unwrap();
            refs.insert(format!("refs/remotes/origin/{branch}"), oid.to_owned());
        }
        if refs.contains_key("refs/remotes/origin/HEAD") {
            let head = self.git(&bare, &["rev-parse", "HEAD"]);
            refs.insert("refs/remotes/origin/HEAD".into(), head);
        }
        for (r, oid) in changes {
            refs.insert((*r).to_owned(), (*oid).to_owned());
        }
        refs
    }

    /// Where the visibility check reads repos by default: a `file://` dir
    /// under the tempdir, `anon/<account>/<name>`, which holds nothing until
    /// a test puts a repo there (`publish_anonymously`) — so no check ever
    /// leaves the machine.
    pub fn visibility_base(&self) -> String {
        format!("file://{}/", self.anonymous_dir().display())
    }

    /// The dir `visibility_base` names.
    pub fn anonymous_dir(&self) -> PathBuf {
        self.base.as_path().join("anon")
    }

    /// Makes `name`'s bare remote readable where the visibility check looks,
    /// as `anon/<OWNER>/<name>`: a repo anyone can read.
    pub fn publish_anonymously(&self, name: &str) {
        let dir = self.anonymous_dir().join(OWNER);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(self.bare(name), dir.join(name)).unwrap();
        // what the check will read: the bare remote's HEAD
        let url = format!("{}{OWNER}/{name}", self.visibility_base());
        assert!(
            self.git(self.base(), &["ls-remote", &url, "HEAD"])
                .ends_with("\tHEAD")
        );
    }

    /// The unregistered scan over the workspace root, with the registry's
    /// entries and owners as the binary loads them.
    pub fn unregistered(&self) -> Vec<UnregisteredClone> {
        let entries = self.entries();
        let registry = ValidRegistry::load(&self.root().join(REGISTRY_FILE)).unwrap();
        scan_unregistered(&self.root(), &entries, registry.owners(), &self.runner())
            .unwrap()
            .unregistered
    }

    /// One entry's status, from a run over every entry.
    pub fn entry(&self, key: &str) -> EntryStatus {
        take_entry(self.status(), key)
    }

    // --- setup assertions ---

    /// Whether `rev` resolves in `repo`.
    pub fn has_ref(&self, repo: &Path, rev: &str) -> bool {
        self.git_output(repo, &["rev-parse", "--verify", "-q", rev])
            .status
            .success()
    }

    /// Asserts `%(upstream:track)` for a local branch: `""` (even or no
    /// upstream), `"[ahead 1]"`, `"[gone]"`, ….
    pub fn assert_track(&self, repo: &Path, branch: &str, track: &str) {
        let got = self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(upstream:track)",
                &format!("refs/heads/{branch}"),
            ],
        );
        assert_eq!(got, track, "track of {branch} in {}", repo.display());
    }

    /// Asserts a local branch's resolved upstream (`""` for none).
    pub fn assert_upstream(&self, repo: &Path, branch: &str, upstream: &str) {
        let got = self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(upstream)",
                &format!("refs/heads/{branch}"),
            ],
        );
        assert_eq!(got, upstream, "upstream of {branch} in {}", repo.display());
    }

    /// Asserts `rev-list --count <args>`.
    pub fn assert_count(&self, repo: &Path, args: &[&str], n: u32) {
        let mut all = vec!["rev-list", "--count"];
        all.extend(args);
        let got: u32 = self.git(repo, &all).parse().unwrap();
        assert_eq!(got, n, "rev-list --count {}", args.join(" "));
    }

    /// Asserts the checkout's `status --porcelain` lines, sorted.
    pub fn assert_porcelain(&self, repo: &Path, lines: &[&str]) {
        let out = self.git_raw(repo, &["status", "--porcelain", "--untracked-files=normal"]);
        let mut got: Vec<&str> = out.lines().collect();
        got.sort_unstable();
        let mut want = lines.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "status of {}", repo.display());
    }

    /// Asserts the checkout is clean.
    pub fn assert_clean(&self, repo: &Path) {
        self.assert_porcelain(repo, &[]);
    }

    /// Asserts whether the repo is shallow.
    pub fn assert_shallow(&self, repo: &Path, shallow: bool) {
        let got = self.git(repo, &["rev-parse", "--is-shallow-repository"]);
        assert_eq!(got, shallow.to_string(), "shallow {}", repo.display());
    }

    /// The time of the repo's `git clone` entry, as git reads it: the oldest
    /// in `HEAD`'s reflog, asserted to be a clone's.
    pub fn clone_reflog_time(&self, repo: &Path) -> u64 {
        let log = self.git(
            repo,
            &["reflog", "show", "--date=unix", "--format=%gd %gs", "HEAD"],
        );
        let oldest = log.lines().last().unwrap();
        let (selector, subject) = oldest.split_once(' ').unwrap();
        assert!(subject.starts_with("clone: from "), "{oldest}");
        selector
            .strip_prefix("HEAD@{")
            .and_then(|s| s.strip_suffix('}'))
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Asserts which branch HEAD is on (`None` when detached).
    pub fn assert_head(&self, repo: &Path, branch: Option<&str>) {
        let out = self.git_output(repo, &["symbolic-ref", "-q", "--short", "HEAD"]);
        let got = out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        assert_eq!(got.as_deref(), branch, "HEAD of {}", repo.display());
    }
}

/// The `origin` an owned entry expects: SSH.
pub fn owned_origin(name: &str) -> String {
    format!("git@github.com:{OWNER}/{name}")
}

/// The `origin` a third-party reference expects: HTTPS.
pub fn third_party_origin(name: &str) -> String {
    format!("https://github.com/{THIRD_PARTY}/{name}")
}

/// Writes a file under `dir`, creating parents.
pub fn write(dir: &Path, path: &str, content: &str) {
    let path = dir.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// A log file's lines; none when it doesn't exist.
fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Writes an executable file, creating parents — from a child process, so
/// this one never holds it open for writing.
///
/// The tests run in threads of one process, and every spawn forks it: a
/// fork taken while this process holds a write fd on the file keeps a copy
/// until the child execs (`O_CLOEXEC` closes it only then), and an exec of
/// the file meanwhile — the test's next git call running it, or git
/// running it as `ssh` — fails with `ETXTBSY` ("Text file busy"). A
/// rename can't help (the copy is of the same inode), and a retry would
/// have to wrap spawns git makes itself. A child's fds are its own, and
/// all closed once it's waited for.
pub fn write_executable(dir: &Path, path: &str, content: &str) {
    use std::io::Write as _;
    let path = dir.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "writing {}",
        path.display()
    );
}

/// The entry keyed `key`, from a status run.
pub fn take_entry(entries: Vec<EntryStatus>, key: &str) -> EntryStatus {
    entries
        .into_iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no entry `{key}` in the report"))
}

/// The entry keyed `key`.
pub fn find_entry<'a>(entries: &'a [EntryStatus], key: &str) -> &'a EntryStatus {
    entries
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no entry `{key}` in the report"))
}

/// An entry's unprobed worktrees, as the probe's facts (without what
/// `classify` decided about pruning them).
pub fn unprobed_facts(entry: &EntryStatus) -> Vec<UnprobedWorktree> {
    entry
        .unprobed_worktrees
        .iter()
        .map(|u| u.worktree.clone())
        .collect()
}

/// The branch named `name` in an entry's report.
pub fn branch<'a>(entry: &'a EntryStatus, name: &str) -> &'a BranchStatus {
    entry
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch `{name}` in {}: {:#?}", entry.key, entry.branches))
}

/// The branch names an entry reports, sorted.
pub fn branch_names(entry: &EntryStatus) -> Vec<&str> {
    let mut names: Vec<&str> = entry.branches.iter().map(|b| b.name.as_str()).collect();
    names.sort_unstable();
    names
}

/// Every file and dir under a git dir with its length, mtime, and inode —
/// compared around a tool call to show it wrote nothing there. The inode
/// catches a same-length rewrite within one timestamp tick: git writes
/// through a lock file renamed into place.
pub type GitDirSnapshot = BTreeMap<PathBuf, (u64, SystemTime, u64)>;

/// Snapshots `git_dir` (a `.git`, or a linked worktree's
/// `.git/worktrees/<name>`), recursively.
pub fn snapshot_git_dir(git_dir: &Path) -> GitDirSnapshot {
    fn walk(root: &Path, dir: &Path, out: &mut GitDirSnapshot) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (meta.len(), meta.modified().unwrap(), meta.ino()),
            );
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = GitDirSnapshot::new();
    walk(git_dir, git_dir, &mut out);
    out
}

/// Asserts two snapshots of one git dir are equal, naming what changed.
pub fn assert_git_dir_unchanged(before: &GitDirSnapshot, after: &GitDirSnapshot) {
    let changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(changed.is_empty(), "the git dir changed: {changed:?}");
}

/// Restores a path's permissions on drop, so the tempdir can be deleted
/// whether or not the test passes.
#[derive(Debug)]
pub struct Unseal(pub PathBuf);

impl Drop for Unseal {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Sets `path`'s mode, restored on the guard's drop; `None` (already
/// restored) when permissions don't bind this user (root), so the caller
/// skips its test.
pub fn seal(path: &Path, mode: u32) -> Option<Unseal> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    let guard = Unseal(path.to_owned());
    let binds = if path.is_dir() {
        std::fs::read_dir(path).is_err()
    } else {
        std::fs::File::open(path).is_err()
    };
    if binds {
        Some(guard)
    } else {
        eprintln!("skipped: permissions don't bind this user (root)");
        None
    }
}

// --- live sessions ---

/// A child process of the test, alive until dropped: a real pid a session
/// file can name.
#[derive(Debug)]
pub struct LiveChild(std::process::Child);

impl LiveChild {
    /// A child where the test runs.
    pub fn spawn() -> Self {
        Self::spawn_in(&std::env::current_dir().unwrap())
    }

    /// A child whose cwd is `dir`: what `/proc/<pid>/cwd` names.
    pub fn spawn_in(dir: &Path) -> Self {
        let child = Command::new("sleep")
            .arg("600")
            .current_dir(dir)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let live = Self(child);
        assert!(Path::new(&format!("/proc/{}", live.pid())).exists());
        live
    }

    pub fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Its `starttime`, from `/proc/<pid>/stat`, as a session file records
    /// it (`procStart`).
    pub fn proc_start(&self) -> String {
        proc_start(self.pid())
    }
}

impl Drop for LiveChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A pid no process has: a child's, once it's been reaped.
pub fn dead_pid() -> u32 {
    let mut child = Command::new("true").stdin(Stdio::null()).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "pid {pid} reused"
    );
    pid
}

/// A live pid's `starttime`.
pub fn proc_start(pid: u32) -> String {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat_starttime(&stat).unwrap().to_string()
}

/// This machine's pid domain as Claude Code records it:
/// `linux:<machine id>:pid:[<pid namespace inode>]`.
pub fn own_pid_domain() -> String {
    let id = std::fs::read_to_string("/etc/machine-id").unwrap();
    let ns = std::fs::read_link("/proc/self/ns/pid").unwrap();
    format!("linux:{}:{}", id.trim(), ns.display())
}

/// A Claude Code config dir: `sessions/<pid>.json` files and a
/// `daemon/roster.json`.
#[derive(Debug)]
pub struct ClaudeDir(pub PathBuf);

impl ClaudeDir {
    /// An empty config dir at `dir` (no `sessions/` yet).
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// Writes `sessions/<name>` as given.
    pub fn write_raw(&self, name: &str, content: &str) -> PathBuf {
        let dir = self.0.join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Writes a session file as Claude Code does, in this machine's pid
    /// domain, with the fields the reader ignores beside the ones it reads.
    pub fn session(&self, pid: u32, proc_start: &str, cwd: &Path) -> PathBuf {
        self.session_in(pid, proc_start, cwd, &own_pid_domain())
    }

    /// Writes a session file recorded in `pid_domain`.
    pub fn session_in(&self, pid: u32, proc_start: &str, cwd: &Path, pid_domain: &str) -> PathBuf {
        let doc = serde_json::json!({
            "pid": pid,
            "sessionId": "00000000-0000-0000-0000-000000000000",
            "cwd": cwd.to_str().unwrap(),
            "startedAt": 1_790_000_000_000_u64,
            "procStart": proc_start,
            "version": "2.1.284",
            "kind": "interactive",
            "pidDomain": pid_domain,
            "status": "idle",
        });
        self.write_raw(&format!("{pid}.json"), &doc.to_string())
    }

    /// Writes `daemon/roster.json` as given.
    pub fn roster(&self, doc: &serde_json::Value) -> PathBuf {
        let dir = self.0.join("daemon");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roster.json");
        std::fs::write(&path, doc.to_string()).unwrap();
        path
    }
}

/// A roster worker as the daemon records it (no pid domain), its session
/// process `repl`: a pid and its `starttime`.
pub fn roster_worker(
    pid: u32,
    proc_start: &str,
    cwd: &Path,
    (repl_pid, repl_proc_start): (u32, &str),
) -> serde_json::Value {
    serde_json::json!({
        "pid": pid,
        "procStart": proc_start,
        "sessionId": "00000000-0000-0000-0000-000000000001",
        "cliVersion": "2.1.284",
        "cwd": cwd.to_str().unwrap(),
        "replPid": repl_pid,
        "replProcStart": repl_proc_start,
    })
}

/// Sets a file's mtime, so its stat info no longer matches the index.
pub fn set_mtime(path: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}
