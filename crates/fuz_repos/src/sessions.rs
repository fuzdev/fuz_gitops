//! The live Claude Code sessions on this machine, for busy detection:
//! `busy` scopes them to the checkouts they sit in.
//!
//! Claude Code's formats are read here, the reason it writes on a worktree
//! lock included (`claude_lock`), which `busy` matches against the live
//! sessions.
//!
//! **The reader** (`read_live_sessions`) reads every config dir it's given
//! — `$CLAUDE_CONFIG_DIR` and `~/.claude`, the same dir once — listing
//! `sessions/` and opening only `<pid>.json` files, plus the background
//! workers in `daemon/roster.json`, deduplicated by pid and cwd (a session
//! file's record wins over a worker's, whichever dir holds it). Both are
//! Claude Code's internal formats, so they parse leniently: unknown fields
//! are ignored, and only `pid`, `procStart`, `cwd` (absolute), a session
//! file's `pidDomain`, and a worker's `replPid`, `replProcStart`, and
//! `worktreePath` (absolute; and `pidDomain`, when present) are read. A
//! session is live iff `/proc/<pid>` exists and its `starttime` (field 22 of
//! `/proc/<pid>/stat`) equals the recorded `procStart`, which defeats pid
//! reuse. Dead entries — the roster keeps exited workers — are ignored, as
//! is a file that doesn't parse when no process has its pid: nothing live
//! could be behind it. Claude Code rewrites these files in place, so one
//! that can't be read or parsed is read once more after a moment before it
//! counts.
//!
//! **Where a session works** is more than its recorded `cwd`, Claude Code's
//! `originalCwd`: the dir it was launched in, rewritten when it enters or
//! exits a worktree or resumes a session, but never moved by the Bash
//! tool's `cd`. A worker dispatched with worktree isolation records the
//! worktree too (`worktreePath`). And each live session carries its
//! process's cwd as `/proc/<pid>/cwd` names it, when that isn't the
//! recorded one, read before its liveness is checked (so a pid reused
//! since can't lend its cwd): Claude Code moves its process into a worktree
//! it enters, which the rewritten `cwd` mostly says already, so it's a
//! defense more than a signal of its own. Both are additive: a link that
//! can't be read (another user's process, a non-dumpable one, or one that
//! just exited), names a dir since removed, or isn't UTF-8 adds nothing,
//! and the recorded cwd still applies.
//!
//! **The caller** is excluded — the session `$CLAUDE_PID` names, and a
//! roster worker whose `replPid` it is — but only when that pid is an
//! ancestor of this process (`caller_ancestors`, by the ppid chain in
//! `/proc`). Anything else can set the variable, and a claim the process
//! tree doesn't back excludes nothing.
//!
//! **Fail closed.** No sessions dir means nothing is live. But a live
//! session the reader can't vouch for makes detection `Unavailable`, and
//! then every push, fast-forward, and move is held: a file that doesn't
//! parse while a process has its pid, a `pidDomain` (machine id and pid
//! namespace) other than the tool's own, where `/proc` can't speak for its
//! pid, a file or dir that can't be read, `HOME` unset (Claude Code falls
//! back to the passwd entry's home, which the tool doesn't read, so a
//! `CLAUDE_CONFIG_DIR` alone isn't every dir), a config dir given as a
//! relative path (which the tool's cwd would resolve, not the session's), a
//! session's cwd (or worktree, or process cwd) that can't be resolved (when
//! `busy` scopes it), or no `/proc` at all (not Linux, or not mounted) while
//! anything is recorded. A format change degrades `sync` to fetching; it
//! never silently drops the guard.
//!
//! **Limits.** A Claude process that writes no session file and is no
//! roster worker is invisible — an out-of-process agent-team teammate, or
//! an interactive session started inside another session's environment —
//! and so is one whose `CLAUDE_CONFIG_DIR` names a dir the tool doesn't
//! read.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::regular_file::read_bounded_bytes;

/// The largest session file or roster the reader reads; a larger one
/// can't be read.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How long the reader waits before reading a file that couldn't be read or
/// parsed once more: long enough for an in-place rewrite to land.
const TORN_READ_RETRY: Duration = Duration::from_millis(50);

/// How many ancestors `caller_ancestors` walks up at most.
const MAX_ANCESTRY: usize = 64;

/// `ESRCH`: reading `/proc/<pid>/stat` raced the process's exit.
const ESRCH: i32 = 3;

/// A live session: a Claude Code process on this machine, and where it
/// works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub pid: u32,
    /// Its process's `starttime` (field 22 of `/proc/<pid>/stat`), as
    /// recorded and checked live: what a worktree lock Claude Code wrote
    /// names beside its pid (`busy`). Not in the report.
    #[serde(skip)]
    pub proc_start: u64,
    /// Its working directory as recorded, Claude Code's `originalCwd`:
    /// where it was launched, rewritten when it enters or exits a worktree
    /// or resumes a session. The Bash tool's `cd` doesn't move it.
    pub cwd: String,
    /// The worktree a roster worker was dispatched into (its recorded
    /// `worktreePath`), where it works too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Where its process is now (`/proc/<pid>/cwd`), when that's not `cwd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_cwd: Option<String>,
    pub source: SessionSource,
}

impl Session {
    /// A session as recorded, at `cwd` alone.
    pub const fn at(pid: u32, proc_start: u64, cwd: String, source: SessionSource) -> Self {
        Self {
            pid,
            proc_start,
            cwd,
            worktree: None,
            process_cwd: None,
            source,
        }
    }

    /// Every path it works in: its recorded cwd, its worktree, and its
    /// process's cwd.
    pub fn places(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.cwd.as_str())
            .chain(self.worktree.as_deref())
            .chain(self.process_cwd.as_deref())
    }
}

/// Where a session was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    /// `sessions/<pid>.json`.
    SessionFile,
    /// A worker in `daemon/roster.json` — as a live session's source, one
    /// recorded by no session file with its pid and cwd.
    RosterWorker,
}

/// Why busy detection can't vouch for every live session — so every push,
/// fast-forward, and move is held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Unavailable {
    /// `HOME` isn't set (or is empty). Claude Code then falls back to the
    /// home the passwd entry names, which the tool doesn't read, so where
    /// sessions are recorded is unknown — whatever `CLAUDE_CONFIG_DIR` says,
    /// since `~/.claude` is read beside it.
    HomeUnknown,
    /// A config dir (`CLAUDE_CONFIG_DIR`, or `HOME`'s `.claude`) isn't an
    /// absolute path: the tool's cwd would resolve it, which says nothing
    /// of where Claude Code's sessions resolved it.
    RelativeConfigDir { path: String },
    /// A dir or file the reader needs couldn't be read (twice, for a file):
    /// a sessions dir, a session file or the roster while a process has its
    /// pid (a dir, a special file, or one over the size cap included), a
    /// `/proc/<pid>/stat`, or what the tool's own pid domain is read from
    /// (`/etc/machine-id`, `/proc/self/ns/pid`) — `/proc/self/stat` when
    /// there's no `/proc` to check pids against. Or a live session's cwd
    /// couldn't be resolved: `path` is as far as it got, such as a component
    /// in a dir the tool can't search, or a symlink loop.
    Unreadable { path: String, error: String },
    /// A session file (or roster worker) for a live pid, or the roster
    /// itself, isn't the format the reader knows, read twice.
    Unparseable { path: String, error: String },
    /// A session file (or roster worker) recorded in another pid domain —
    /// another machine, or another pid namespace — where `/proc` can't
    /// speak for its pid. `source` says which: a session file names one
    /// session, the roster many.
    ForeignPidDomain {
        path: String,
        pid_domain: String,
        source: SessionSource,
    },
}

/// What the reader found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSessions {
    /// Every live session but the caller's, by pid and cwd.
    Known(Vec<Session>),
    Unavailable(Unavailable),
}

/// Where the reader looks, and whom it excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsSource {
    /// Claude Code's config dirs, each read (`config_dirs`), or why they
    /// can't be told.
    pub config_dirs: Result<Vec<PathBuf>, Unavailable>,
    /// The calling session's pid as `CLAUDE_PID` claims it.
    pub claude_pid: Option<u32>,
    /// This process's ancestors' pids and start times
    /// (`caller_ancestors`): `claude_pid` excludes a session only when it's
    /// one of them.
    pub ancestors: BTreeMap<u32, u64>,
}

impl SessionsSource {
    /// From the environment: `CLAUDE_CONFIG_DIR` and `$HOME/.claude` (an
    /// empty value counts as unset), `CLAUDE_PID` (ignored unless it's a
    /// pid), and this process's ancestors.
    pub fn from_env() -> Self {
        let set = |name| std::env::var_os(name).filter(|v| !v.is_empty());
        Self {
            config_dirs: config_dirs(set("CLAUDE_CONFIG_DIR"), set("HOME")),
            claude_pid: set("CLAUDE_PID").and_then(|v| v.to_str().and_then(parse_pid)),
            ancestors: caller_ancestors(),
        }
    }

    /// The caller's pid and start time: `claude_pid`, when it's an
    /// ancestor.
    fn caller(&self) -> Option<(u32, u64)> {
        let pid = self.claude_pid?;
        self.ancestors.get(&pid).map(|&start| (pid, start))
    }
}

/// Who runs the tool: a person, or an agent — a Claude Code agent shell,
/// which sets `CLAUDECODE`.
///
/// An agent's pushes are held (`HeldBy::Gateway`): agents push through the
/// gateway, `repos push` with its policy, and until it lands a person runs
/// `repos sync` to push. The hold lifts with it. Guidance, not a boundary —
/// an agent can unset the variable; the host's rules are the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Person,
    Agent,
}

impl Caller {
    /// From the environment: `Agent` when `CLAUDECODE` is set (an empty
    /// value counts as unset).
    pub fn from_env() -> Self {
        Self::from_claudecode(std::env::var_os("CLAUDECODE").as_deref())
    }

    /// From `CLAUDECODE`'s value, as `from_env` reads it.
    pub fn from_claudecode(value: Option<&std::ffi::OsStr>) -> Self {
        if value.is_some_and(|v| !v.is_empty()) {
            Self::Agent
        } else {
            Self::Person
        }
    }
}

/// The config dirs to read: `config_dir` (`CLAUDE_CONFIG_DIR`), when it's
/// given, and `home`'s `.claude`, the same dir once.
///
/// Dirs are compared canonicalized, as given when they can't be. A relative
/// one is kept as given, never merged into another: the reader refuses it.
///
/// # Errors
///
/// `HomeUnknown` without a `home`: Claude Code would fall back to the
/// passwd entry's home, which isn't read.
pub fn config_dirs(
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<Vec<PathBuf>, Unavailable> {
    let home = home.ok_or(Unavailable::HomeUnknown)?;
    let candidates = config_dir
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from(home).join(".claude")));
    let mut seen = BTreeSet::new();
    Ok(candidates
        .filter(|dir| {
            dir.is_relative() || seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.clone()))
        })
        .collect())
}

/// This process's ancestors by pid, each with its `starttime`, read from
/// `/proc` (`walk_ancestors`).
pub fn caller_ancestors() -> BTreeMap<u32, u64> {
    read_stat(Path::new("/proc/self/stat")).map_or_else(BTreeMap::new, |own| {
        walk_ancestors(own, |pid| {
            read_stat(&PathBuf::from(format!("/proc/{pid}/stat")))
        })
    })
}

/// The ppid chain up from a process whose ppid and `starttime` are `own`,
/// by pid, each with its `starttime`: `stat` reads a pid's ppid and
/// `starttime`.
///
/// Up to and including pid 1, the namespace's init — a Claude Code running
/// as pid 1 in a container is its own sessions' ancestor — but not pid 0 (a
/// parent outside the pid namespace), at most `MAX_ANCESTRY` of them. The
/// walk stops early where `stat` can't read a pid or a parent reads as
/// started after its child (the parent exited and its pid was reused):
/// coming up short only excludes less.
fn walk_ancestors(own: (u32, u64), stat: impl Fn(u32) -> Option<(u32, u64)>) -> BTreeMap<u32, u64> {
    let mut ancestors = BTreeMap::new();
    let (mut ppid, mut child_start) = own;
    for _ in 0..MAX_ANCESTRY {
        if ppid == 0 {
            break;
        }
        let Some((next, start)) = stat(ppid) else {
            break;
        };
        if start > child_start {
            break;
        }
        ancestors.insert(ppid, start);
        if ppid == 1 {
            break;
        }
        (ppid, child_start) = (next, start);
    }
    ancestors
}

/// A `/proc/<pid>/stat`'s ppid and `starttime`.
fn read_stat(path: &Path) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(path).ok()?;
    Some((stat_ppid(&stat)?, stat_starttime(&stat)?))
}

/// A pid written as plain decimal digits.
fn parse_pid(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The pid a session file's name carries: `<pid>.json`, digits only.
fn session_file_pid(name: &str) -> Option<u32> {
    name.strip_suffix(".json").and_then(parse_pid)
}

/// The fields of a `/proc/<pid>/stat` line from field 3 on. The command
/// name (field 2) is parenthesized and may itself hold spaces and parens,
/// so fields are counted from after the last `)`.
fn stat_fields(stat: &str) -> Option<std::str::SplitAsciiWhitespace<'_>> {
    let (_, rest) = stat.rsplit_once(')')?;
    Some(rest.split_ascii_whitespace())
}

/// Field 22 (`starttime`) of a `/proc/<pid>/stat` line.
pub fn stat_starttime(stat: &str) -> Option<u64> {
    stat_fields(stat)?.nth(22 - 3)?.parse().ok()
}

/// Field 4 (`ppid`) of a `/proc/<pid>/stat` line.
fn stat_ppid(stat: &str) -> Option<u32> {
    stat_fields(stat)?.nth(4 - 3)?.parse().ok()
}

/// A session file's fields the reader needs.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRecord {
    pid: u32,
    proc_start: String,
    cwd: String,
    pid_domain: String,
}

/// A roster worker's fields the reader needs beside its `pid`, which is
/// read first. The roster doesn't record a pid domain; it's checked when
/// present.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerRecord {
    proc_start: String,
    cwd: String,
    #[serde(default)]
    pid_domain: Option<String>,
    /// The worker's session process, which `CLAUDE_PID` names inside it.
    #[serde(default)]
    repl_pid: Option<u32>,
    /// That process's `starttime`, matched against the caller's when
    /// recorded.
    #[serde(default)]
    repl_proc_start: Option<String>,
    /// The worktree it was dispatched into, when it was given one.
    #[serde(default)]
    worktree_path: Option<String>,
}

/// `daemon/roster.json`: its workers, by id, each read leniently on its
/// own.
#[derive(Debug, Deserialize)]
struct Roster {
    workers: BTreeMap<String, serde_json::Value>,
}

/// Whether a pid names a live process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    Live,
    Dead,
}

/// The reader's state: what it found, and what it read of the machine,
/// once.
struct Reader {
    /// By pid and cwd: a worker with a session file's pid but another cwd
    /// holds its own checkout too.
    live: BTreeMap<(u32, String), Session>,
    /// Checked on first need: without `/proc`, every pid would read dead.
    proc_checked: bool,
    own_domain: Option<String>,
    /// Waits out an in-place rewrite before a read is retried
    /// (`retry_torn`): `TORN_READ_RETRY`, but a seam for tests.
    pause: Box<dyn FnMut()>,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            live: BTreeMap::new(),
            proc_checked: false,
            own_domain: None,
            pause: Box::new(|| std::thread::sleep(TORN_READ_RETRY)),
        }
    }
}

type Step<T> = Result<T, Unavailable>;

fn unreadable(path: &Path, error: &std::io::Error) -> Unavailable {
    Unavailable::Unreadable {
        path: path.to_string_lossy().into_owned(),
        error: error.to_string(),
    }
}

fn unparseable(path: &Path, error: &dyn std::fmt::Display) -> Unavailable {
    Unavailable::Unparseable {
        path: path.to_string_lossy().into_owned(),
        error: error.to_string(),
    }
}

impl Reader {
    /// `read`, and once more after the pause when what it read couldn't be
    /// read or parsed: Claude Code rewrites its files in place, so a read
    /// can land mid-write.
    fn retry_torn<T>(&mut self, mut read: impl FnMut(&mut Self) -> Step<T>) -> Step<T> {
        match read(self) {
            Err(Unavailable::Unreadable { .. } | Unavailable::Unparseable { .. }) => {
                (self.pause)();
                read(self)
            }
            done => done,
        }
    }

    /// Fails unless `/proc` can speak for pids.
    fn check_proc(&mut self) -> Step<()> {
        if !self.proc_checked {
            let path = Path::new("/proc/self/stat");
            let stat = std::fs::read_to_string(path).map_err(|e| unreadable(path, &e))?;
            if stat_starttime(&stat).is_none() {
                return Err(unparseable(path, &"no starttime field"));
            }
            self.proc_checked = true;
        }
        Ok(())
    }

    /// Whether any process has `pid` — a file that doesn't parse matters
    /// only then.
    fn pid_exists(&mut self, pid: u32) -> Step<bool> {
        self.check_proc()?;
        let path = PathBuf::from(format!("/proc/{pid}"));
        match std::fs::metadata(&path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(unreadable(&path, &e)),
        }
    }

    /// Whether `pid` is the process that started at `proc_start`.
    fn liveness(&mut self, pid: u32, proc_start: u64) -> Step<Liveness> {
        self.check_proc()?;
        let path = PathBuf::from(format!("/proc/{pid}/stat"));
        let stat = match std::fs::read_to_string(&path) {
            Ok(stat) => stat,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Liveness::Dead),
            Err(e) if e.raw_os_error() == Some(ESRCH) => return Ok(Liveness::Dead),
            Err(e) => return Err(unreadable(&path, &e)),
        };
        let starttime =
            stat_starttime(&stat).ok_or_else(|| unparseable(&path, &"no starttime field"))?;
        Ok(if starttime == proc_start {
            Liveness::Live
        } else {
            Liveness::Dead
        })
    }

    /// The tool's own pid domain, as Claude Code writes it:
    /// `linux:<machine id>:pid:[<namespace inode>]`.
    fn own_domain(&mut self) -> Step<&str> {
        if self.own_domain.is_none() {
            let id_path = Path::new("/etc/machine-id");
            let id = std::fs::read_to_string(id_path).map_err(|e| unreadable(id_path, &e))?;
            let ns_path = Path::new("/proc/self/ns/pid");
            let ns = std::fs::read_link(ns_path).map_err(|e| unreadable(ns_path, &e))?;
            self.own_domain = Some(format!("linux:{}:{}", id.trim(), ns.to_string_lossy()));
        }
        Ok(self.own_domain.as_deref().unwrap_or_default())
    }

    /// Fails unless `domain`, recorded at `path` by `source`, is the tool's
    /// own.
    fn check_domain(&mut self, path: &Path, source: SessionSource, domain: &str) -> Step<()> {
        if self.own_domain()? == domain {
            Ok(())
        } else {
            Err(Unavailable::ForeignPidDomain {
                path: path.to_string_lossy().into_owned(),
                pid_domain: domain.to_owned(),
                source,
            })
        }
    }

    /// Reads one `sessions/<pid>.json`, `pid` from its name: the session,
    /// when it's live.
    fn session_file(&mut self, path: &Path, pid: u32) -> Step<Option<Session>> {
        let text = match read_bounded(path, MAX_FILE_BYTES) {
            Ok(text) => text,
            // gone since the listing: the session ended
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if self.pid_exists(pid)? => return Err(unreadable(path, &e)),
            Err(_) => return Ok(None),
        };
        let record = serde_json::from_str::<SessionRecord>(&text)
            .map_err(|e| e.to_string())
            .and_then(|r| {
                if r.pid == pid {
                    Ok(r)
                } else {
                    Err(format!("records pid {} under its name's {pid}", r.pid))
                }
            })
            .and_then(|r| check_absolute("cwd", &r.cwd).map(|()| r))
            .and_then(|r| parse_proc_start(&r.proc_start).map(|start| (r, start)));
        let (record, proc_start) = match record {
            Ok(parsed) => parsed,
            // nothing live can be behind a file whose pid no process has
            Err(e) if self.pid_exists(pid)? => return Err(unparseable(path, &e)),
            Err(_) => return Ok(None),
        };
        // before liveness: another domain's pid means nothing to this `/proc`
        self.check_domain(path, SessionSource::SessionFile, &record.pid_domain)?;
        let process_cwd = process_cwd(pid, &record.cwd);
        Ok(
            (self.liveness(pid, proc_start)? == Liveness::Live).then_some(Session {
                process_cwd,
                ..Session::at(pid, proc_start, record.cwd, SessionSource::SessionFile)
            }),
        )
    }

    /// Reads `daemon/roster.json`'s live workers but the one whose session
    /// process is `caller`.
    fn roster(&mut self, path: &Path, caller: Option<(u32, u64)>) -> Step<Vec<Session>> {
        let text = match read_bounded(path, MAX_FILE_BYTES) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(unreadable(path, &e)),
        };
        let roster: Roster = serde_json::from_str(&text).map_err(|e| unparseable(path, &e))?;
        let mut live = Vec::new();
        for (id, value) in roster.workers {
            let at = |e: String| unparseable(path, &format!("worker {id}: {e}"));
            // without a pid, whether it's alive can't be told
            let pid = value
                .get("pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
                .ok_or_else(|| at("no pid".to_owned()))?;
            let record = serde_json::from_value::<WorkerRecord>(value)
                .map_err(|e| e.to_string())
                .and_then(|r| check_absolute("cwd", &r.cwd).map(|()| r))
                .and_then(|r| match &r.worktree_path {
                    Some(worktree) => check_absolute("worktreePath", worktree).map(|()| r),
                    None => Ok(r),
                })
                .and_then(|r| parse_proc_start(&r.proc_start).map(|start| (r, start)));
            let (record, proc_start) = match record {
                Ok(parsed) => parsed,
                Err(e) if self.pid_exists(pid)? => return Err(at(e)),
                Err(_) => continue,
            };
            if let Some(domain) = &record.pid_domain {
                self.check_domain(path, SessionSource::RosterWorker, domain)?;
            }
            if caller.is_some_and(|c| record.is_callers(c)) {
                continue;
            }
            let process_cwd = process_cwd(pid, &record.cwd);
            if self.liveness(pid, proc_start)? == Liveness::Live {
                live.push(Session {
                    worktree: record.worktree_path,
                    process_cwd,
                    ..Session::at(pid, proc_start, record.cwd, SessionSource::RosterWorker)
                });
            }
        }
        Ok(live)
    }

    /// Reads one config dir's session files and roster.
    fn config_dir(&mut self, config_dir: &Path, caller: Option<(u32, u64)>) -> Step<()> {
        let dir = config_dir.join("sessions");
        match std::fs::read_dir(&dir) {
            Ok(listing) => {
                let mut files = Vec::new();
                for item in listing {
                    let item = item.map_err(|e| unreadable(&dir, &e))?;
                    let name = item.file_name();
                    if let Some(pid) = name.to_str().and_then(session_file_pid) {
                        files.push((pid, item.path()));
                    }
                }
                // in pid order: the first failure is the same every run
                files.sort();
                for (pid, path) in files {
                    if let Some(s) = self.retry_torn(|r| r.session_file(&path, pid))? {
                        self.add(s);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unreadable(&dir, &e)),
        }
        let roster = config_dir.join("daemon/roster.json");
        for s in self.retry_torn(|r| r.roster(&roster, caller))? {
            self.add(s);
        }
        Ok(())
    }

    /// Adds a live session, one per pid and cwd: a session file's record
    /// replaces a roster worker's, whichever config dir was read first,
    /// keeping the worker's worktree.
    fn add(&mut self, session: Session) {
        match self.live.entry((session.pid, session.cwd.clone())) {
            Entry::Vacant(slot) => {
                slot.insert(session);
            }
            Entry::Occupied(mut slot) => {
                let kept = slot.get_mut();
                let worktree = kept.worktree.take().or_else(|| session.worktree.clone());
                if session.source == SessionSource::SessionFile {
                    *kept = session;
                }
                kept.worktree = worktree;
            }
        }
    }

    fn read(&mut self, source: &SessionsSource) -> Step<()> {
        let dirs = source.config_dirs.as_ref().map_err(Clone::clone)?;
        if let Some(dir) = dirs.iter().find(|d| d.is_relative()) {
            return Err(Unavailable::RelativeConfigDir {
                path: dir.to_string_lossy().into_owned(),
            });
        }
        let caller = source.caller();
        for dir in dirs {
            self.config_dir(dir, caller)?;
        }
        Ok(())
    }
}

impl WorkerRecord {
    /// Whether the worker's session process is `caller` (pid, start time):
    /// its `replPid`, and its `replProcStart` when recorded.
    fn is_callers(&self, (pid, start): (u32, u64)) -> bool {
        self.repl_pid == Some(pid)
            && self
                .repl_proc_start
                .as_deref()
                .is_none_or(|s| parse_proc_start(s) == Ok(start))
    }
}

/// A recorded path, `field`, which must be absolute: a relative one says
/// nothing of where the session works.
fn check_absolute(field: &str, path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(format!("{field} {path:?} isn't absolute"))
    }
}

/// Where process `pid` is now, from `/proc/<pid>/cwd`, when that isn't
/// `cwd` (as recorded, or resolved). `None` when the link can't be read
/// (another user's process, a non-dumpable one, one that just exited),
/// isn't UTF-8, or names a dir since removed (the kernel's ` (deleted)`
/// suffix, on a path that isn't there).
fn process_cwd(pid: u32, cwd: &str) -> Option<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    if link.as_os_str().as_bytes().ends_with(b" (deleted)")
        && std::fs::symlink_metadata(&link).is_err()
    {
        return None;
    }
    let recorded = Path::new(cwd);
    if link == recorded || recorded.canonicalize().is_ok_and(|real| real == link) {
        return None;
    }
    link.into_os_string().into_string().ok()
}

/// A `procStart` as recorded: decimal digits.
fn parse_proc_start(s: &str) -> Result<u64, String> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().map_err(|e| format!("procStart {s:?}: {e}"))
    } else {
        Err(format!("procStart {s:?} isn't a number"))
    }
}

/// A regular file's contents, at most `max` bytes, as UTF-8.
fn read_bounded(path: &Path, max: u64) -> std::io::Result<String> {
    String::from_utf8(read_bounded_bytes(path, max)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Every live Claude Code session on this machine but the caller's, or why
/// that can't be vouched for.
pub fn read_live_sessions(source: &SessionsSource) -> LiveSessions {
    let mut reader = Reader::default();
    match reader.read(source) {
        Ok(()) => {
            let caller = source.caller().map(|(pid, _)| pid);
            LiveSessions::Known(
                reader
                    .live
                    .into_values()
                    .filter(|s| Some(s.pid) != caller)
                    .collect(),
            )
        }
        Err(reason) => LiveSessions::Unavailable(reason),
    }
}

/// What a worktree lock Claude Code wrote names: its process's pid, and
/// that process's `starttime` when the lock gives one (`busy`'s module doc,
/// **Claude Code's worktree locks**).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeLock<'a> {
    pid: u64,
    start: Option<&'a str>,
}

impl ClaudeLock<'_> {
    /// Whether it names `session`: the same pid, and the same start time to
    /// the digit when it gives one, as Claude Code compares them.
    pub(crate) fn names(&self, session: &Session) -> bool {
        self.pid == u64::from(session.pid)
            && self
                .start
                .is_none_or(|start| start == session.proc_start.to_string())
    }
}

/// A lock reason as Claude Code's own parser reads it, a JavaScript regex:
///
/// ```text
/// ^claude (?:agent|session) .{1,255} \(pid (\d{1,10})(?: start (.{1,255}))?\)$
/// ```
///
/// `None` when it doesn't match: no lock of Claude Code's. The name is
/// greedy, so of the ` (pid `s in the reason the last that leaves a
/// matching tail wins.
pub(crate) fn claude_lock(reason: &str) -> Option<ClaudeLock<'_>> {
    let rest = reason.strip_prefix("claude ")?;
    let rest = rest
        .strip_prefix("agent ")
        .or_else(|| rest.strip_prefix("session "))?;
    rest.rmatch_indices(" (pid ").find_map(|(i, sep)| {
        if !is_js_dots(&rest[..i]) {
            return None;
        }
        let body = rest[i + sep.len()..].strip_suffix(')')?;
        let digits = body.bytes().take_while(u8::is_ascii_digit).count();
        if !(1..=10).contains(&digits) {
            return None;
        }
        let (pid, after) = body.split_at(digits);
        let start = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(" start ").filter(|s| is_js_dots(s))?)
        };
        Some(ClaudeLock {
            pid: pid.parse().ok()?,
            start,
        })
    })
}

/// Whether the JavaScript regex `.{1,255}` (no `u` flag) matches all of
/// `s`: 1 to 255 UTF-16 code units, none a line terminator.
fn is_js_dots(s: &str) -> bool {
    // a UTF-16 unit is at most 3 UTF-8 bytes, so a longer `s` is over 255
    // units: rejecting it unscanned keeps `claude_lock`'s parse linear
    if s.len() > 3 * 255 {
        return false;
    }
    let units: usize = s.chars().map(char::len_utf16).sum();
    (1..=255).contains(&units)
        && !s
            .chars()
            .any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
}

#[cfg(test)]
mod tests {
    use std::path::Component;

    use super::*;

    #[test]
    fn claudecode_set_is_an_agent() {
        let caller = |v: Option<&str>| Caller::from_claudecode(v.map(std::ffi::OsStr::new));
        assert_eq!(caller(Some("1")), Caller::Agent);
        assert_eq!(caller(Some("0")), Caller::Agent);
        assert_eq!(caller(Some("")), Caller::Person);
        assert_eq!(caller(None), Caller::Person);
    }

    #[test]
    fn starttime_is_field_22_after_the_last_paren() {
        let tail = "S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 50111550 5980160 434";
        assert_eq!(
            stat_starttime(&format!("3291533 (cat) {tail}")),
            Some(50_111_550)
        );
        // a name with spaces and parens of its own
        assert_eq!(
            stat_starttime(&format!("7 (a ) b) (c)) {tail}")),
            Some(50_111_550)
        );
        assert_eq!(
            stat_starttime(&format!("7 (x) y) {tail}")),
            Some(50_111_550)
        );
        // too few fields, no name, a field that isn't a number
        assert_eq!(stat_starttime("7 (cat) S 1 2 3"), None);
        assert_eq!(stat_starttime(tail), None);
        assert_eq!(
            stat_starttime("7 (cat) S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 x 5"),
            None
        );
    }

    #[test]
    fn ppid_is_field_4_after_the_last_paren() {
        let tail = "S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 50111550 5980160 434";
        assert_eq!(stat_ppid(&format!("3291533 (cat) {tail}")), Some(1));
        assert_eq!(stat_ppid(&format!("7 (a ) 9 (c)) {tail}")), Some(1));
        assert_eq!(stat_ppid("7 (cat) S"), None);
        assert_eq!(stat_ppid("7 (cat) S x"), None);
    }

    #[test]
    fn the_ancestors_are_the_ppid_chain() {
        let ancestors = caller_ancestors();
        let parent = std::os::unix::process::parent_id();
        let stat = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        assert_eq!(
            ancestors.get(&parent).copied(),
            stat_starttime(&stat(parent)),
            "{ancestors:?}"
        );
        assert!(!ancestors.contains_key(&std::process::id()));
        // each one's parent is the next, up to pid 1 — itself included when
        // the chain reaches it (not when a parent is outside the namespace)
        let mut pid = parent;
        while pid > 1 {
            assert!(ancestors.contains_key(&pid), "{pid}: {ancestors:?}");
            pid = stat_ppid(&stat(pid)).unwrap();
        }
        if pid == 1 {
            assert_eq!(
                ancestors.get(&1).copied(),
                stat_starttime(&stat(1)),
                "{ancestors:?}"
            );
        } else {
            assert!(!ancestors.contains_key(&1), "{ancestors:?}");
        }
    }

    /// Walks up from a process whose ppid is 50 and started at 500, over
    /// `chain` (pid to ppid and start time); a pid it lacks can't be read.
    fn walk(chain: &[(u32, (u32, u64))]) -> BTreeMap<u32, u64> {
        let chain: BTreeMap<u32, (u32, u64)> = chain.iter().copied().collect();
        walk_ancestors((50, 500), |pid| {
            assert!(pid > 0, "looked up pid {pid}");
            chain.get(&pid).copied()
        })
    }

    #[test]
    fn the_ancestor_walk_follows_ppids_while_they_hold() {
        // up to and including pid 1, never past it, whatever its ppid reads
        let to_init = [
            (50, (40, 400)),
            (40, (30, 300)),
            (30, (1, 1)),
            (1, (7, 0)),
            (7, (0, 0)),
        ];
        assert_eq!(
            walk(&to_init),
            BTreeMap::from([(50, 400), (40, 300), (30, 1), (1, 0)])
        );
        // a Claude Code running as pid 1, this process's parent
        assert_eq!(
            walk_ancestors((1, 500), |pid| {
                assert_eq!(pid, 1, "looked up pid {pid}");
                Some((7, 0))
            }),
            BTreeMap::from([(1, 0)])
        );
        // not pid 0, a parent outside the pid namespace
        let to_zero = [(50, (40, 400)), (40, (0, 300))];
        assert_eq!(walk(&to_zero), BTreeMap::from([(50, 400), (40, 300)]));
        assert!(walkfrom((0, 500)).is_empty());
        // a parent started after its child: its pid was reused, so neither
        // it nor anything above it is an ancestor
        let reused = [(50, (40, 400)), (40, (30, 401)), (30, (20, 1))];
        assert_eq!(walk(&reused), BTreeMap::from([(50, 400)]));
        let reused_first = [(50, (40, 501))];
        assert!(walk(&reused_first).is_empty());
        // started in the same tick as its child: still its parent
        let same_tick = [(50, (40, 500)), (40, (1, 500))];
        assert_eq!(walk(&same_tick), BTreeMap::from([(50, 500), (40, 500)]));
        // a pid that can't be read stops the walk
        let unreadable = [(50, (40, 400)), (30, (1, 1))];
        assert_eq!(walk(&unreadable), BTreeMap::from([(50, 400)]));
        // a chain that never ends is cut at the cap
        let endless = walk_ancestors((1000, 0), |pid| Some((pid + 1, 0)));
        assert_eq!(endless.len(), MAX_ANCESTRY);
        assert_eq!(
            endless.keys().max(),
            Some(&(1000 + u32::try_from(MAX_ANCESTRY).unwrap() - 1))
        );
    }

    /// Walks up from `own` over a chain with nothing in it.
    fn walkfrom(own: (u32, u64)) -> BTreeMap<u32, u64> {
        walk_ancestors(own, |pid| panic!("looked up pid {pid}"))
    }

    /// A reader whose pause counts itself instead of sleeping.
    fn counting_reader() -> (Reader, std::rc::Rc<std::cell::Cell<usize>>) {
        let pauses = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = std::rc::Rc::clone(&pauses);
        let reader = Reader {
            pause: Box::new(move || counter.set(counter.get() + 1)),
            ..Reader::default()
        };
        (reader, pauses)
    }

    #[test]
    fn a_torn_read_is_retried_once() {
        let unreadable = || Unavailable::Unreadable {
            path: "/x".into(),
            error: "e".into(),
        };
        let unparseable = || Unavailable::Unparseable {
            path: "/x".into(),
            error: "e".into(),
        };
        // `results(n)` is the nth call's: the calls made, the pauses, and
        // what came of it
        let run = |results: &dyn Fn(usize) -> Step<u8>| {
            let (mut reader, pauses) = counting_reader();
            let mut calls = 0;
            let got = reader.retry_torn(|_| {
                calls += 1;
                results(calls)
            });
            (calls, pauses.get(), got)
        };
        assert_eq!(run(&|_| Ok(7)), (1, 0, Ok(7)));
        // torn, then whole
        for torn in [unreadable(), unparseable()] {
            let whole = |n| if n == 1 { Err(torn.clone()) } else { Ok(7) };
            assert_eq!(run(&whole), (2, 1, Ok(7)));
            // torn for good: read twice, and it counts
            assert_eq!(run(&|_| Err(torn.clone())), (2, 1, Err(torn.clone())));
        }
        // nothing to wait out
        for reason in [
            Unavailable::HomeUnknown,
            Unavailable::RelativeConfigDir { path: "x".into() },
            Unavailable::ForeignPidDomain {
                path: "/x".into(),
                pid_domain: "d".into(),
                source: SessionSource::SessionFile,
            },
        ] {
            assert_eq!(run(&|_| Err(reason.clone())), (1, 0, Err(reason.clone())));
        }
    }

    #[test]
    fn a_file_rewritten_during_the_pause_is_read_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude");
        let pid = std::process::id();
        let start = read_stat(Path::new("/proc/self/stat"))
            .unwrap()
            .1
            .to_string();
        let domain = Reader::default().own_domain().unwrap().to_owned();
        let source = SessionsSource {
            config_dirs: Ok(vec![dir.clone()]),
            claude_pid: None,
            ancestors: BTreeMap::new(),
        };
        let session_file = dir.join(format!("sessions/{pid}.json"));
        let roster = dir.join("daemon/roster.json");
        let here = std::env::current_dir()
            .unwrap()
            .canonicalize()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap();
        assert_ne!(here, "/");
        let docs = [
            (
                &session_file,
                serde_json::json!({
                    "pid": pid, "procStart": start, "cwd": "/", "pidDomain": domain,
                }),
                SessionSource::SessionFile,
            ),
            (
                &roster,
                serde_json::json!({"workers": {"w": {"pid": pid, "procStart": start, "cwd": "/"}}}),
                SessionSource::RosterWorker,
            ),
        ];
        for (file, doc, recorded_as) in docs {
            let whole = doc.to_string();
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, &whole[..whole.len() / 2]).unwrap();
            // half-written, then whole once the reader pauses
            let (mut reader, pauses) = counting_reader();
            let (path, rewrite) = (file.clone(), whole.clone());
            let mut count = reader.pause;
            reader.pause = Box::new(move || {
                count();
                std::fs::write(&path, &rewrite).unwrap();
            });
            assert_eq!(reader.read(&source), Ok(()));
            assert_eq!(pauses.get(), 1);
            // this process isn't at `/`: where it is joins the session
            assert_eq!(
                reader.live.into_values().collect::<Vec<_>>(),
                [Session {
                    process_cwd: Some(here.clone()),
                    ..Session::at(pid, start.parse().unwrap(), "/".into(), recorded_as)
                }]
            );
            // left half-written: read twice, and it can't be vouched for
            std::fs::write(file, &whole[..whole.len() / 2]).unwrap();
            let (mut reader, pauses) = counting_reader();
            assert!(
                matches!(reader.read(&source), Err(Unavailable::Unparseable { path, .. })
                    if path == file.to_string_lossy()),
            );
            assert_eq!(pauses.get(), 1);
            std::fs::remove_file(file).unwrap();
        }
    }

    #[test]
    fn the_caller_is_an_ancestor_or_nobody() {
        let source = |claude_pid| SessionsSource {
            config_dirs: Ok(vec![]),
            claude_pid,
            ancestors: BTreeMap::from([(10, 100), (20, 50)]),
        };
        assert_eq!(source(Some(10)).caller(), Some((10, 100)));
        assert_eq!(source(Some(11)).caller(), None);
        assert_eq!(source(None).caller(), None);
        let worker = |repl_pid, repl_proc_start: Option<&str>| WorkerRecord {
            proc_start: "1".into(),
            cwd: "/".into(),
            pid_domain: None,
            repl_pid,
            repl_proc_start: repl_proc_start.map(Into::into),
            worktree_path: None,
        };
        assert!(worker(Some(10), Some("100")).is_callers((10, 100)));
        assert!(worker(Some(10), None).is_callers((10, 100)));
        // the pid reused, or unreadable: not the caller
        assert!(!worker(Some(10), Some("101")).is_callers((10, 100)));
        assert!(!worker(Some(10), Some("x")).is_callers((10, 100)));
        assert!(!worker(Some(11), Some("100")).is_callers((10, 100)));
        assert!(!worker(None, None).is_callers((10, 100)));
    }

    #[test]
    fn config_dirs_are_both_once_each() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let other = base.join("other");
        let dirs = |config: Option<&Path>, home: Option<&Path>| {
            config_dirs(
                config.map(|p| p.as_os_str().to_owned()),
                home.map(|p| p.as_os_str().to_owned()),
            )
        };
        assert_eq!(
            dirs(Some(&other), Some(&home)),
            Ok(vec![other.clone(), home.join(".claude")])
        );
        assert_eq!(dirs(None, Some(&home)), Ok(vec![home.join(".claude")]));
        // without a home, `~/.claude` can't be found: a `CLAUDE_CONFIG_DIR`
        // alone isn't every dir
        assert_eq!(dirs(Some(&other), None), Err(Unavailable::HomeUnknown));
        assert_eq!(dirs(None, None), Err(Unavailable::HomeUnknown));
        // the same dir, however spelled, is read once
        let link = base.join("link");
        std::os::unix::fs::symlink(home.join(".claude"), &link).unwrap();
        assert_eq!(dirs(Some(&link), Some(&home)), Ok(vec![link.clone()]));
        assert_eq!(
            dirs(Some(&home.join(".claude/.")), Some(&home)).map(|d| d.len()),
            Ok(1)
        );
        // a relative one is kept as given, for the reader to refuse
        let relative = Path::new("rel");
        assert_eq!(
            dirs(Some(relative), Some(&home)),
            Ok(vec![relative.to_owned(), home.join(".claude")])
        );
        assert_eq!(
            dirs(Some(&link), Some(relative)),
            Ok(vec![link, relative.join(".claude")])
        );
    }

    #[test]
    fn a_relative_config_dir_naming_a_read_one_is_refused_not_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        let claude = home.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        // `..` up to `/` from the tool's cwd, then the absolute path: from
        // here it names the very dir `home` does
        let cwd = std::env::current_dir().unwrap();
        let up: PathBuf = cwd
            .components()
            .skip(1)
            .map(|_| Component::ParentDir)
            .collect();
        let relative = up.join(claude.strip_prefix("/").unwrap());
        assert!(relative.is_relative());
        assert_eq!(relative.canonicalize().unwrap(), claude);
        let relative_home = up.join(home.strip_prefix("/").unwrap());
        for (config, home) in [(relative, home), (claude, relative_home)] {
            let dirs = config_dirs(Some(config.into_os_string()), Some(home.into_os_string()));
            assert_eq!(dirs.as_ref().map(Vec::len), Ok(2), "{dirs:?}");
            let source = SessionsSource {
                config_dirs: dirs,
                claude_pid: None,
                ancestors: BTreeMap::new(),
            };
            assert!(
                matches!(
                    read_live_sessions(&source),
                    LiveSessions::Unavailable(Unavailable::RelativeConfigDir { path })
                        if Path::new(&path).is_relative()
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn session_files_are_pid_dot_json() {
        assert_eq!(session_file_pid("123.json"), Some(123));
        for name in [
            "123.abc.key",
            "123.json.tmp",
            "+123.json",
            "-1.json",
            ".json",
            "abc.json",
            "123",
            "99999999999.json",
        ] {
            assert_eq!(session_file_pid(name), None, "{name}");
        }
    }

    #[test]
    fn records_parse_leniently_but_need_their_fields() {
        let full = r#"{"pid":7,"procStart":"42","cwd":"/ws/app","pidDomain":"d",
            "status":"idle","somethingNew":{"x":1}}"#;
        let r: SessionRecord = serde_json::from_str(full).unwrap();
        assert_eq!(
            (r.pid, r.proc_start.as_str(), r.cwd.as_str()),
            (7, "42", "/ws/app")
        );
        let no_domain = r#"{"pid":7,"procStart":"42","cwd":"/ws/app"}"#;
        assert!(serde_json::from_str::<SessionRecord>(no_domain).is_err());
        // a worker records no domain
        let w: WorkerRecord = serde_json::from_str(no_domain).unwrap();
        assert_eq!(
            (w.pid_domain, w.repl_pid, w.repl_proc_start, w.worktree_path),
            (None, None, None, None)
        );
        assert!(parse_proc_start("42").is_ok());
        for bad in ["", "-1", "4 2", "0x2a"] {
            assert!(parse_proc_start(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_lock_reason_is_read_as_claude_code_reads_it() {
        let lock = |pid, start| Some(ClaudeLock { pid, start });
        for (reason, want) in [
            (
                "claude agent agent-a1 (pid 42 start 123)",
                lock(42, Some("123")),
            ),
            ("claude agent agent-a1 (pid 42)", lock(42, None)),
            (
                "claude session feat/x (pid 42 start 123)",
                lock(42, Some("123")),
            ),
            // the name takes anything but a line break, and is greedy
            ("claude agent a b) (c (pid 42)", lock(42, None)),
            (
                "claude agent a (pid 1) b (pid 42 start 7)",
                lock(42, Some("7")),
            ),
            ("claude agent a (pid 1 start 2) (pid 3)", lock(3, None)),
            // the last ` (pid ` leaves `9))`, so an earlier one wins
            (
                "claude agent x (pid 5 start 7 (pid 9))",
                lock(5, Some("7 (pid 9)")),
            ),
            // `\d{1,10}`, taken as a number
            ("claude agent a (pid 0042)", lock(42, None)),
            ("claude agent a (pid 9999999999)", lock(9_999_999_999, None)),
            ("claude agent a (pid 42 start x y)", lock(42, Some("x y"))),
        ] {
            assert_eq!(claude_lock(reason), want, "{reason:?}");
        }
        for reason in [
            "",
            "claude agent a (pid )",
            "claude agent a (pid 12345678901)",
            "claude agent  (pid 42)",
            "claude agent a (pid 42 start )",
            "claude agent a (pid 42 start 7",
            "claude agent a (pid 42) ",
            "claude agent a (pid 42)\n",
            "claude agent a\nb (pid 42)",
            "claude agent a\rb (pid 42)",
            "claude agent a (pid 42 start 7\u{2028})",
            "claude agent a (pid 42 start 7\u{2029})",
            "claude agent a (pid -42)",
            "claude agent a (pid 42,start 7)",
            "claude worker a (pid 42)",
            "claude  agent a (pid 42)",
            "agent a (pid 42)",
        ] {
            assert_eq!(claude_lock(reason), None, "{reason:?}");
        }
        // 1 to 255 UTF-16 code units each
        let name = "n".repeat(255);
        assert!(claude_lock(&format!("claude agent {name} (pid 42)")).is_some());
        assert!(claude_lock(&format!("claude agent {name}n (pid 42)")).is_none());
        let astral = "\u{1F600}".repeat(127);
        assert!(claude_lock(&format!("claude agent {astral} (pid 42 start {astral})")).is_some());
        let astral = "\u{1F600}".repeat(128);
        assert!(claude_lock(&format!("claude agent {astral} (pid 42)")).is_none());
        assert!(claude_lock(&format!("claude agent a (pid 42 start {astral})")).is_none());
        // 3 UTF-8 bytes to the unit, the most bytes 255 units can take
        let wide = "\u{20AC}".repeat(255);
        assert!(claude_lock(&format!("claude agent {wide} (pid 42 start {wide})")).is_some());
        assert!(claude_lock(&format!("claude agent {wide}\u{20AC} (pid 42)")).is_none());
    }

    #[test]
    fn a_huge_lock_reason_is_rejected_in_linear_time() {
        // every ` (pid ` is a candidate split, each over an ever longer name
        let reason = format!("claude agent a{}", " (pid 1".repeat(320 * 1024 / 7));
        let started = std::time::Instant::now();
        assert_eq!(claude_lock(&reason), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_lock_names_a_session_by_pid_and_start_time_to_the_digit() {
        let s = Session::at(
            42,
            123,
            "/nonexistent-ws".into(),
            SessionSource::SessionFile,
        );
        let names = |reason: &str| claude_lock(reason).unwrap().names(&s);
        assert!(names("claude agent a (pid 42 start 123)"));
        assert!(names("claude agent a (pid 42)"));
        assert!(names("claude agent a (pid 0042)"));
        assert!(!names("claude agent a (pid 43 start 123)"));
        assert!(!names("claude agent a (pid 43)"));
        assert!(!names("claude agent a (pid 42 start 124)"));
        assert!(!names("claude agent a (pid 42 start 0123)"));
        assert!(!names("claude agent a (pid 42 start 123 )"));
        assert!(!names("claude agent a (pid 4294967338)"));
    }
}
