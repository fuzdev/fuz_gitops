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
