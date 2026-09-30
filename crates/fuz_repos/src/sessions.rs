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
    pub worktree: Option<String>,
    /// Where its process is now (`/proc/<pid>/cwd`), when that's not `cwd`.
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
/// It decides one thing: an agent is refused `repos push --new-branch`
/// (`check_new_branch`), since creating a remote branch is the user's. An
/// agent's `sync` and `push` otherwise run as a person's. Guidance, not a
/// boundary — an agent can unset the variable; the host's rules are the
/// floor.
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
    fn from_claudecode(value: Option<&std::ffi::OsStr>) -> Self {
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
mod tests;
