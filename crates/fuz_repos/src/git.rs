//! The one way this crate runs git: hardened flags and env, a per-call
//! timeout, and capped output capture.
//!
//! Git never runs repo-controlled code on the tool's behalf — hooks and
//! fsmonitor are disabled and background maintenance is off. Optional locks
//! are off too, so observing never rewrites another session's index, and lazy
//! fetching is off, so a local call on a partial clone never touches the
//! network.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use thiserror::Error;

/// Stdout beyond this is an error: no parser here wants a partial record.
const STDOUT_CAP: usize = 64 * 1024 * 1024;
/// Stderr is kept for messages only.
const STDERR_CAP: usize = 64 * 1024;
/// The wait between `SIGTERM` and `SIGKILL` for a timed-out git. git deletes
/// its lock files on `SIGTERM`; a bare `SIGKILL` strands them.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// The timeout for a local call.
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
/// The timeout for a network call.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);
/// SSH's connect timeout under batch mode, in seconds.
const SSH_CONNECT_TIMEOUT_SECS: u32 = 15;

/// Inherited variables that would point git somewhere other than `-C`.
const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

/// The oldest git the runner supports: `GIT_NO_LAZY_FETCH`, which keeps a
/// local call on a partial clone off the network, landed in 2.44.
pub const MIN_GIT_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 44,
    patch: 0,
};

/// A git release, compared by its numeric components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for GitVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl GitVersion {
    /// Parses `git --version`'s output: the first line reading `git version
    /// X.Y.Z` (a wrapper may print a banner first), then anything — a vendor
    /// suffix (`2.47.3.windows.1`, `2.39.5 (Apple Git-154)`), a release
    /// candidate (`2.44.0.rc1`, read as `2.44.0`), a dev build's describe
    /// (`2.43.0.381.gb435a96ce8`, read as `2.43.0`). The patch may be absent
    /// (read as `0`); the major and minor may not.
    pub fn parse(output: &str) -> Option<Self> {
        let version = version_line(output)?;
        let token = version.split_whitespace().next()?;
        let mut parts = token.split('.');
        let mut number = |required: bool| -> Option<u32> {
            let part = parts.next();
            let digits = part.map_or("", |p| {
                let end = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
                &p[..end]
            });
            // a component with no leading digits: required ones fail, the
            // patch reads as `0`
            if digits.is_empty() {
                return if required { None } else { Some(0) };
            }
            digits.parse().ok()
        };
        Some(Self {
            major: number(true)?,
            minor: number(true)?,
            patch: number(false)?,
        })
    }
}

/// The first line of `git --version`'s output reading `git version …`, from
/// after that prefix.
fn version_line(output: &str) -> Option<&str> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("git version "))
}

/// A git call that didn't produce usable output.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("git not found on PATH")]
    NotFound,
    #[error("failed to run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("git {args} timed out after {}s", after.as_secs())]
    Timeout { args: String, after: Duration },
    #[error("git {args} printed more than {cap} bytes")]
    OutputTooLarge { args: String, cap: usize },
    #[error("git {args} failed{}: {stderr}", code.map_or_else(String::new, |c| format!(" ({c})")))]
    Failed {
        args: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("git {args} printed non-UTF-8 output")]
    NonUtf8 { args: String },
}

/// Per-call options.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallOptions<'a> {
    /// Stop repo discovery above this dir (`GIT_CEILING_DIRECTORIES`), so a
    /// dir that isn't a repo never resolves to a parent's.
    pub ceiling: Option<&'a Path>,
    /// A network call: the longer timeout, and batch-mode SSH unless the user
    /// configures SSH themselves.
    pub network: Option<NetworkOptions>,
}

/// Options for a call that reaches a remote.
#[derive(Debug, Clone, Copy)]
pub struct NetworkOptions {
    /// Whether to set `GIT_SSH_COMMAND` to batch mode with a connect timeout.
    /// Off when the repo sets `core.sshCommand` or the env sets
    /// `GIT_SSH_COMMAND`/`GIT_SSH`.
    pub batch_ssh: bool,
}

/// A finished git call.
#[derive(Debug)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// The runner. Shared by reference across the pool; counts its spawns.
#[derive(Debug, Default)]
pub struct Git {
    spawns: AtomicU32,
    /// When set, the only environment git sees besides the runner's own
    /// hardening; `None` inherits the caller's.
    env: Option<Vec<(OsString, OsString)>>,
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner whose git calls see only `env` plus the runner's own
    /// hardening — no inherited variable reaches git, so neither the caller's
    /// `HOME` (and with it the global config and excludes file) nor its
    /// `GIT_*` settings apply. The system config still does unless `env` sets
    /// `GIT_CONFIG_NOSYSTEM`. For hermetic callers like the fixture tests.
    /// git and its own children (ssh, `!` aliases, hooks) all search `env`'s
    /// `PATH`, never the caller's, so `env` should carry one.
    pub const fn with_clean_env(env: Vec<(OsString, OsString)>) -> Self {
        Self {
            spawns: AtomicU32::new(0),
            env: Some(env),
        }
    }

    /// How many git processes this runner has spawned.
    pub fn spawns(&self) -> u32 {
        self.spawns.load(Ordering::Relaxed)
    }

    /// Whether the environment git sees already configures its SSH.
    pub fn env_configures_ssh(&self) -> bool {
        const VARS: [&str; 2] = ["GIT_SSH_COMMAND", "GIT_SSH"];
        self.env.as_ref().map_or_else(
            || VARS.iter().any(|v| std::env::var_os(v).is_some()),
            |env| env.iter().any(|(k, _)| VARS.iter().any(|v| k == v)),
        )
    }

    /// Runs `git -C <dir> <args>` and returns its output whatever the exit
    /// status.
    ///
    /// # Errors
    ///
    /// When git can't be spawned, times out, or overflows the stdout cap.
    pub fn run(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<GitOutput, GitError> {
        let mut cmd = Command::new("git");
        // first, so the hardening below wins over anything the caller sets
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        cmd.args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "maintenance.auto=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        for var in SCRUBBED_ENV {
            cmd.env_remove(var);
        }
        if let Some(ceiling) = opts.ceiling {
            cmd.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let mut timeout = LOCAL_TIMEOUT;
        if let Some(net) = opts.network {
            timeout = NETWORK_TIMEOUT;
            if net.batch_ssh {
                cmd.env(
                    "GIT_SSH_COMMAND",
                    format!("ssh -o BatchMode=yes -o ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
                );
            }
        }

        let child = cmd.spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                GitError::NotFound
            } else {
                GitError::Spawn(e)
            }
        })?;
        self.spawns.fetch_add(1, Ordering::Relaxed);
        wait_capped(child, timeout, || args.join(" "))
    }

    /// Checks that git is on `PATH` and at least `MIN_GIT_VERSION`, with one
    /// `git --version` in `dir`. Run once, before anything else calls git.
    ///
    /// # Errors
    ///
    /// `GitNotFound` when git isn't on `PATH`; `GitTooOld` when its version
    /// is older or unrecognized — the runner can't vouch for a git it can't
    /// place — or when git rejects the runner's own flags (git before 2.15
    /// has no `--no-optional-locks`); `Io` when `git --version` otherwise
    /// fails.
    pub fn check_version(&self, dir: &Path) -> crate::error::Result<GitVersion> {
        use crate::error::Error;
        let out = match self.output_string(dir, &["--version"], CallOptions::default()) {
            Ok(out) => out,
            Err(GitError::NotFound) => return Err(Error::GitNotFound),
            Err(GitError::Failed { stderr, .. })
                if stderr.to_ascii_lowercase().starts_with("unknown option") =>
            {
                return Err(Error::GitTooOld {
                    found: FOUND_PRE_OPTIONAL_LOCKS.into(),
                    required: MIN_GIT_VERSION,
                });
            }
            Err(e) => {
                return Err(Error::Io {
                    context: "failed to run git --version".into(),
                    source: io::Error::other(e),
                });
            }
        };
        match GitVersion::parse(&out) {
            Some(v) if v >= MIN_GIT_VERSION => Ok(v),
            parsed => Err(Error::GitTooOld {
                found: parsed.map_or_else(|| describe_version(&out), |v| v.to_string()),
                required: MIN_GIT_VERSION,
            }),
        }
    }

    /// Runs git and returns stdout, treating a non-zero exit as an error.
    ///
    /// # Errors
    ///
    /// As `run`, plus `Failed` on a non-zero exit.
    pub fn output(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<Vec<u8>, GitError> {
        let out = self.run(dir, args, opts)?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(GitError::Failed {
                args: args.join(" "),
                code: out.status.code(),
                stderr: out.stderr.trim().to_owned(),
            })
        }
    }

    /// As `output`, decoded as UTF-8.
    ///
    /// # Errors
    ///
    /// As `output`, plus `NonUtf8`.
    pub fn output_string(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<String, GitError> {
        String::from_utf8(self.output(dir, args, opts)?).map_err(|_| GitError::NonUtf8 {
            args: args.join(" "),
        })
    }
}

/// `GitTooOld`'s `found` for a git that rejects `--no-optional-locks`, the
/// runner's first flag, before it can print its version.
pub const FOUND_PRE_OPTIONAL_LOCKS: &str = "unknown (older than 2.15)";

/// What an unrecognized `git --version` printed, for the error: its version
/// line without the `git version ` prefix, else its first line; capped.
fn describe_version(output: &str) -> String {
    const CAP: usize = 80;
    let line = version_line(output).unwrap_or_else(|| output.lines().next().unwrap_or(""));
    line.trim().chars().take(CAP).collect()
}

/// Captured bytes, and whether any were dropped over the cap.
type Captured = (Vec<u8>, bool);
/// A thread draining one of the child's pipes.
type Reader = thread::JoinHandle<io::Result<Captured>>;

/// Waits for `child` under `timeout`, draining both pipes on their own
/// threads so neither can fill and block it.
fn wait_capped(
    mut child: Child,
    timeout: Duration,
    args: impl Fn() -> String,
) -> Result<GitOutput, GitError> {
    let stdout = child
        .stdout
        .take()
        .map(|s| thread::spawn(move || read_capped(s, STDOUT_CAP)));
    let stderr = child
        .stderr
        .take()
        .map(|s| thread::spawn(move || read_capped(s, STDERR_CAP)));
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait());
    });

    let Ok(status) = rx.recv_timeout(timeout) else {
        signal(pid, "TERM");
        if rx.recv_timeout(KILL_GRACE).is_err() {
            signal(pid, "KILL");
            let _ = rx.recv();
        }
        // the pipes may stay open in a grandchild (ssh), so the readers are
        // left to finish on their own
        return Err(GitError::Timeout {
            args: args(),
            after: timeout,
        });
    };
    let status = status.map_err(GitError::Spawn)?;

    let join = |h: Option<Reader>| -> Result<Captured, GitError> {
        h.map_or_else(
            || Ok((Vec::new(), false)),
            |h| {
                h.join()
                    .map_err(|_| GitError::Spawn(io::Error::other("output reader panicked")))?
                    .map_err(GitError::Spawn)
            },
        )
    };
    let (stdout, stdout_over) = join(stdout)?;
    let (stderr, _) = join(stderr)?;
    if stdout_over {
        return Err(GitError::OutputTooLarge {
            args: args(),
            cap: STDOUT_CAP,
        });
    }
    Ok(GitOutput {
        status,
        stdout,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Reads up to `cap` bytes, then drains the rest; the flag says whether
/// anything was dropped.
fn read_capped(mut r: impl Read, cap: usize) -> io::Result<Captured> {
    let mut buf = Vec::new();
    r.by_ref().take(cap as u64 + 1).read_to_end(&mut buf)?;
    let over = buf.len() > cap;
    if over {
        buf.truncate(cap);
        io::copy(&mut r, &mut io::sink())?;
    }
    Ok((buf, over))
}

/// Sends a signal by name. std can only `SIGKILL` a child, and the crate
/// forbids `unsafe`, so this goes through `kill(1)`. The pid can't be reused
/// meanwhile: the waiter thread hasn't reaped it.
fn signal(pid: u32, name: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_capped_flags_overflow() {
        let (buf, over) = read_capped(&b"hello"[..], 5).unwrap();
        assert_eq!((buf.as_slice(), over), (&b"hello"[..], false));
        let (buf, over) = read_capped(&b"hello world"[..], 5).unwrap();
        assert_eq!((buf.as_slice(), over), (&b"hello"[..], true));
    }

    #[test]
    fn timeout_terminates_the_child() {
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let e = wait_capped(child, Duration::from_millis(100), || "sleep".into()).unwrap_err();
        assert!(matches!(e, GitError::Timeout { .. }), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    /// Variables the runner sets, plus the ones a shell between git and `env`
    /// would rewrite (none runs for a bare `!env`; skipped as a defence), so
    /// never the same on both sides of the seam.
    const UNSTABLE_ENV: [&str; 7] = [
        "PATH",
        "FIXTURE_VAR",
        "LC_ALL",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ];

    /// The environment a runner's git sees, via a `!` alias.
    fn seen_env(git: &Git, dir: &Path) -> Vec<String> {
        git.output_string(
            dir,
            &["-c", "alias.fixture-env=!env", "fixture-env"],
            CallOptions::default(),
        )
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn a_clean_env_is_all_git_sees_and_the_hardening_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env: Vec<(OsString, OsString)> = vec![
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("FIXTURE_VAR".into(), "kept".into()),
            // each contradicts the runner's hardening
            ("GIT_NO_LAZY_FETCH".into(), "0".into()),
            ("GIT_OPTIONAL_LOCKS".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "1".into()),
            ("LC_ALL".into(), "tr_TR.UTF-8".into()),
            ("GIT_DIR".into(), "/nowhere".into()),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            env.push(("PATH".into(), path));
        }
        let seen = seen_env(&Git::with_clean_env(env), tmp.path());
        let has = |line: &str| seen.iter().any(|l| l == line);
        let has_var = |name: &str| seen.iter().any(|l| l.starts_with(&format!("{name}=")));
        assert!(has("FIXTURE_VAR=kept"), "{seen:?}");
        for hardened in [
            "GIT_NO_LAZY_FETCH=1",
            "GIT_OPTIONAL_LOCKS=0",
            "GIT_TERMINAL_PROMPT=0",
            "LC_ALL=C",
        ] {
            assert!(has(hardened), "{hardened}: {seen:?}");
        }
        // any variable of this process that reaches the alias unchanged
        let inherited = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .find(|(k, v)| {
                !k.starts_with("GIT_") && !UNSTABLE_ENV.contains(&k.as_str()) && !v.contains('\n')
            })
            .map(|(k, v)| format!("{k}={v}"))
            .expect("the test process has an environment");
        // scrubbed even when the caller sets it; not inherited when it doesn't
        assert!(!has_var("GIT_DIR"), "{seen:?}");
        assert!(!has_var("HOME"), "{seen:?}");
        assert!(!has(&inherited), "{inherited}: {seen:?}");

        // control: the default runner inherits the caller's environment
        let seen = seen_env(&Git::new(), tmp.path());
        assert!(seen.contains(&inherited), "{inherited}: {seen:?}");
        assert!(!seen.iter().any(|l| l == "FIXTURE_VAR=kept"));
    }

    #[test]
    fn parses_git_versions() {
        let v = |major, minor, patch| {
            Some(GitVersion {
                major,
                minor,
                patch,
            })
        };
        for (output, expected) in [
            ("git version 2.47.3\n", v(2, 47, 3)),
            ("git version 2.44.0", v(2, 44, 0)),
            ("git version 2.47.3.windows.1\n", v(2, 47, 3)),
            ("git version 2.39.5 (Apple Git-154)\n", v(2, 39, 5)),
            ("git version 2.44.0.rc1", v(2, 44, 0)),
            ("git version 2.43.0.381.gb435a96ce8", v(2, 43, 0)),
            ("git version 2.44", v(2, 44, 0)),
            ("git version 2.44.rc0", v(2, 44, 0)),
            ("git version 10.0.1", v(10, 0, 1)),
            ("  git version 2.50.1  \n", v(2, 50, 1)),
            // a wrapper's banner first
            ("wrapper 1.0 here\ngit version 2.47.3\n", v(2, 47, 3)),
            ("banner\n\n  git version 2.40.1\ntrailer\n", v(2, 40, 1)),
            // not a version
            ("git version", None),
            ("git version 2", None),
            ("git version two.44.0", None),
            ("git version 2.x.0", None),
            ("version 2.44.0", None),
            ("2.44.0", None),
            ("2.44.0\ngit 2.44.0", None),
            ("", None),
            ("hub version 2.44.0", None),
        ] {
            assert_eq!(GitVersion::parse(output), expected, "{output:?}");
        }
    }

    #[test]
    fn git_versions_order_numerically() {
        let parse = |s: &str| GitVersion::parse(&format!("git version {s}")).unwrap();
        assert!(parse("2.43.9") < MIN_GIT_VERSION);
        assert!(parse("2.9.0") < MIN_GIT_VERSION);
        assert!(parse("1.99.99") < MIN_GIT_VERSION);
        assert!(parse("2.44.0") >= MIN_GIT_VERSION);
        assert!(parse("2.100.0") > MIN_GIT_VERSION);
        assert!(parse("3.0.0") > MIN_GIT_VERSION);
        assert_eq!(MIN_GIT_VERSION.to_string(), "2.44.0");
    }

    #[test]
    fn an_unrecognized_version_is_described_capped() {
        assert_eq!(describe_version("git version weird\nmore"), "weird");
        assert_eq!(describe_version("banner\ngit version weird\n"), "weird");
        assert_eq!(describe_version("not git"), "not git");
        assert_eq!(describe_version(&"x".repeat(200)).len(), 80);
    }

    #[test]
    fn this_git_is_new_enough() {
        let tmp = tempfile::tempdir().unwrap();
        let v = Git::new().check_version(tmp.path()).unwrap();
        assert!(v >= MIN_GIT_VERSION, "{v}");
    }

    #[test]
    fn runs_git_in_a_dir() {
        let git = Git::new();
        let tmp = tempfile::tempdir().unwrap();
        let v = git
            .output_string(tmp.path(), &["--version"], CallOptions::default())
            .unwrap();
        assert!(v.starts_with("git version"), "{v}");
        assert_eq!(git.spawns(), 1);
    }
}
