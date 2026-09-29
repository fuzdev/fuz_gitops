//! Busy detection: the checkouts the live Claude Code sessions (`sessions`)
//! sit in.
//!
//! `sync` acts, and agents may run it: without a guard one agent would
//! fast-forward a working tree another live session is editing, or push its
//! commits mid-stream. So a live session marks busy the checkout it works
//! in, and `classify` holds every action on the branches a busy checkout
//! has checked out, pushes included.
//!
//! A session works in each of its places (`Session::places`): its recorded
//! cwd, a roster worker's worktree, and its process's cwd — each scoped as
//! below, and the checkouts they mark joined.
//!
//! **Scope** (`scope_sessions`): a live session marks busy the checkout a
//! place of it sits in — the longest path-component prefix over every
//! checkout probed, linked worktrees included, both sides resolved as the
//! kernel would (a component that doesn't exist, such as a deleted dir,
//! taken as written; one that can't be looked up, such as behind a dir the
//! tool can't search or in a symlink loop, fails) — and a tie, one checkout
//! that's two entries' (a worktree of one at another's dir), marks both. It
//! marks busy too the checkout whose git dir git would find from it
//! (**Attribution**, below), and the checkouts where Claude Code would put
//! its agent worktrees (**Claude Code's own worktrees**, below), and the
//! checkouts whose lock names it (**Claude Code's worktree locks**, below).
//! A session none of these place — at the workspace root, outside every checkout, or
//! in an entry that wasn't probed — is unscoped and never blocks: ff-only
//! already protects uncommitted work, and a push only moves committed refs.
//!
//! A checkout whose path can't be resolved (`UnresolvedCheckout`, also a
//! `needs_human` reason) drops out of the scoping and may be busy:
//! `classify` holds every action on the branches checked out there, as for
//! a busy one — all of them when its HEAD is unknown. That's sound: a
//! session really inside it has a place that can't be resolved either (and
//! the run fails closed), or one that resolves into a shallower checkout
//! (which it then holds) or into none (unscoped) — and all that session
//! could have at stake is the checkout's own branch, which the hold covers.
//! Checkouts are resolved on every run, live sessions or none, so the
//! report doesn't change with whether another session happens to be
//! running.
//!
//! **Claude Code's own worktrees** (`nested_worktrees`): a subagent with
//! worktree isolation runs in its parent's process, so the session file
//! keeps the parent's cwd while the subagent commits in the worktree, which
//! the prefix scoping can't see. Claude Code puts it at
//! `<root>/.claude/worktrees/<name>`, `root` found from the session's cwd:
//! the toplevel git finds there, mapped from a linked worktree to its
//! repo's — the dir holding the common dir when that's named `.git` (the
//! primary checkout), else the common dir itself (a bare repo, or a
//! `--separate-git-dir` one) — unless the worktree's link back to its git
//! dir doesn't name it (moved by hand), when it's the toplevel itself. So
//! each entry a session is placed in has its repo's root (`claude_root`),
//! and a place attributed through a `.git` that isn't a linked worktree's
//! at its listed path has that dir too — a primary's toplevel, a moved or
//! copied worktree's, an unlisted git dir's. The session marks busy every
//! checkout, probed or unprobed, under each root's `.claude/worktrees/`
//! (resolved, by path component) — those paths exactly: never a subdir's,
//! which Claude Code doesn't use, nor a linked worktree's own. A session
//! in a `--separate-git-dir` primary holds the git dir's too, which Claude
//! Code roots only its linked worktrees' sessions at.
//!
//! **Attribution** (`attributed`): a checkout's files needn't be at its
//! path. A worktree moved with a plain `mv` (git lists it prunable at the
//! old path, and keeps working at the new one), copied with `cp -a`, on
//! media mounted somewhere else, or with a `gitdir` naming no path (its
//! only path is then its own git dir) holds files the prefix scoping can't
//! see. So each live session is also placed the way git discovers its
//! repo: walking up from each resolved place to `/`, at each dir the `.git`
//! there — a dir, or a gitfile's `gitdir: ` path, relative to its dir — or
//! else the dir itself (git takes a git dir for a bare repo, so a session
//! inside one works in it). The nearest naming a checkout's own git dir
//! marks that checkout busy: a linked worktree's
//! `<commondir>/worktrees/<id>`, probed or not, or a primary's (a
//! `--separate-git-dir` primary's `.git` is a file naming it). That's sound
//! because git finds a worktree no other way: a session that can commit
//! through one, moving the branch checked out there, has that worktree's
//! `.git` (or git dir) on its walk up, so it's attributed to it wherever
//! its files are. A session placed both ways — a worktree moved into
//! another checkout's tree — is busy in both.
//!
//! A git dir no worktree list names can still share a repo's refs: made by
//! hand, with a `commondir` file naming the repo's common dir, or by
//! `git-new-workdir`, whose `.git` symlinks `refs` into the original's (or
//! by hand with only `refs/heads` symlinked). A session working through one
//! is on that entry's `unlisted`, and `classify` holds the branch its
//! `HEAD` names as though busy — every branch when that `HEAD` is unknown.
//! Git reads a `commondir` and a `HEAD` whole, however large, and takes
//! each as a C string (cut at the first NUL; with none, trailing line
//! breaks trimmed from a `commondir`, trailing whitespace from a `HEAD`),
//! and so does the walk — reading each only up to its first NUL. A file
//! with no NUL in its first `MAX_GIT_C_STRING_BYTES` is the tool's own
//! limit, not git's: such a `commondir` is passed over, where git might
//! follow it (such a `HEAD` is unknown, which holds every branch).
//!
//! Everything else at a `.git` is passed over, and the walk goes on. Git
//! passes over a `.git` it can't look up (a dir it can't search, a symlink
//! loop) or that's no dir or regular file, and stops with an error at a
//! gitfile it can't read, over its 1 MiB limit, or not starting `gitdir: `,
//! or naming no git dir — so no session commits through any of them. The
//! parser reads a gitfile exactly as git does (raw bytes, at most git's
//! 1 MiB, trailing line breaks trimmed, cut at a NUL), so every gitfile git
//! follows is followed. A git dir the probe doesn't know is passed over as
//! the prefix scoping passes over repos it doesn't know (a session in a
//! repo nested in a moved worktree is still in the worktree's files).
//! Walking on only attributes more, and a `.git` never makes detection
//! unavailable. The walk is one stat per level, a bounded read of a `.git`
//! file, and a few lookups in a git dir the probe doesn't know.
//!
//! **Claude Code's worktree locks** (`claude_lock`): Claude Code locks each
//! worktree it creates or resumes by name (`EnterWorktree`, a subagent's),
//! and one a background session adopts as it starts, with the reason
//! `claude <agent|session> <name> (pid <pid> start <start>)`: its own
//! process's pid and `starttime` (field 22 of `/proc/<pid>/stat`, as a
//! session file's `procStart` records it), ` start <start>` left out where
//! it has none. A checkout whose lock names a live session is busy with
//! it, wherever the session's places are: probed or unprobed (a missing
//! worktree's lock is what git keeps it for), the primary included. The
//! reason is read as Claude Code's own parser reads it, and names a
//! session as Claude Code's own liveness check would: the pid is one of
//! the reader's live sessions — so never the caller, and only a process
//! the reader vouched for, which Claude Code's is, the session's own — and
//! a start, when given, is that session's `starttime` to the digit, so a
//! lock left behind by a process whose pid was since reused names no one.
//! A lock naming no live session is passed over. The reasons are the
//! worktree list's, which the probe reads anyway; a worktree git doesn't
//! list has none. Entering an existing worktree by path doesn't lock it:
//! that session is placed by its rewritten cwd.
//!
//! **Limits.** Only discovery from a session's places is seen: a session
//! pointing git elsewhere (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`,
//! `-C`, `--git-dir`) is placed by its places alone — the limit an
//! unscoped session carries too — as are its edits by absolute path.
//! Claude Code roots agent worktrees at its tracked cwd, which the Bash
//! tool's `cd` moves without moving the process or the session file, so a
//! session launched at the workspace root can have agent worktrees in a
//! repo no place of it roots: those are caught by their lock alone.
//! Worktrees Claude Code doesn't lock — a `WorktreeCreate` hook's, or any
//! other tool's — are seen only as checkouts a place sits in. A lock names
//! a session only when the reader sees it, so the locks of a Claude process
//! the reader doesn't see are passed over (see `sessions`' **Limits**). And
//! the lock rule is pinned to Claude Code's current reason format: a change
//! to it silently drops that signal, which can't fail closed, since a
//! reason is free text anyone can write.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::porcelain::is_object_id;
use crate::sessions::{LiveSessions, Session, Unavailable, read_bounded_bytes};
use crate::state::UnprobedHead;

/// The largest `.git` file git reads (`read_gitfile_gently`); git refuses a
/// larger one, so the attribution walk passes it over.
const MAX_GITFILE_BYTES: u64 = 1024 * 1024;

/// The most of a git dir's `commondir` or `HEAD` the attribution walk reads
/// looking for a NUL, where git takes the file as a C string. Git reads
/// either whole, however large, so this is the tool's own limit, not git's:
/// a larger one with no NUL in reach is passed over (a `commondir`) or
/// `Unknown` (a `HEAD`), where git might follow it.
const MAX_GIT_C_STRING_BYTES: u64 = 1024 * 1024;

/// How much of a file `read_c_string` reads at a time.
const C_STRING_CHUNK: usize = 8 * 1024;

/// Busy detection as the report carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sessions {
    /// Every live session was vouched for. Those in a checkout are on it
    /// (`busy`): by path, by the git dir git would find from a place of
    /// theirs, as the parent of a worktree under the `.claude/worktrees/`
    /// Claude Code would root theirs at, or by the lock Claude Code put on
    /// it for them;
    /// those working through a git dir no worktree list names that shares an
    /// entry's refs are on that entry's `unlisted_git_dir` reason.
    /// `unscoped` are the rest, by pid and cwd — at the workspace root,
    /// outside it, or in no checkout the run probed (with targets, other
    /// entries' included). They never block.
    Available { unscoped: Vec<Session> },
    /// Some live session couldn't be vouched for: every push, fast-forward,
    /// and move is held, as though every checkout were busy.
    Unavailable { reason: Unavailable },
}

/// Whether busy detection vouched for every live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    Available,
    Unavailable,
}

/// A checkout whose path couldn't be resolved, so whether a live session
/// works in it can't be told: it may be busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCheckout {
    /// As far as resolving it got: a component in a dir the tool can't
    /// search, or a symlink loop.
    pub path: String,
    pub error: String,
}

/// A git dir no worktree list names that shares an entry's refs, and the
/// live sessions working through it.
///
/// Made by hand, with a `commondir` file naming the entry's common dir, or
/// by `git-new-workdir`, with a `refs` symlinked to the common dir's (or a
/// `refs/heads`). A commit there moves the entry's branch its `HEAD` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistedGitDir {
    /// Its `HEAD`, read as git would; `Unknown` when it can't be read or
    /// names no branch or commit (or is a symlink, git's oldest form), so it
    /// might be on any branch.
    pub head: UnprobedHead,
    pub busy: Vec<Session>,
}

/// The live sessions in one entry's checkouts, what `classify` holds on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySessions {
    pub detection: Detection,
    /// By checkout path, as the probe's facts spell it: the primary's,
    /// each probed worktree's, each unprobed one's.
    pub busy: BTreeMap<String, Vec<Session>>,
    /// Its checkouts whose paths couldn't be resolved, however detection
    /// went, keyed like `busy`.
    pub unresolved: BTreeMap<String, UnresolvedCheckout>,
    /// Git dirs outside its worktree list sharing its refs, a live session
    /// in each, by canonical path.
    pub unlisted: BTreeMap<String, UnlistedGitDir>,
}

impl EntrySessions {
    /// Detection available, and no session in any checkout.
    pub const fn idle() -> Self {
        Self::empty(Detection::Available)
    }

    /// Detection unavailable.
    pub const fn unavailable() -> Self {
        Self::empty(Detection::Unavailable)
    }

    /// No session, no checkout unresolved, and no unlisted git dir.
    const fn empty(detection: Detection) -> Self {
        Self {
            detection,
            busy: BTreeMap::new(),
            unresolved: BTreeMap::new(),
            unlisted: BTreeMap::new(),
        }
    }

    /// The sessions in the checkout at `path`.
    pub fn at(&self, path: &str) -> &[Session] {
        self.busy.get(path).map_or(&[], Vec::as_slice)
    }

    /// Whether the checkout at `path` couldn't be resolved, so it may be
    /// busy.
    pub fn unresolved_at(&self, path: &str) -> bool {
        self.unresolved.contains_key(path)
    }

    /// How many unlisted git dirs may be on `branch`: each whose `HEAD`
    /// names it or is unknown.
    pub fn unlisted_on(&self, branch: &str) -> usize {
        self.unlisted
            .values()
            .filter(|u| match &u.head {
                UnprobedHead::Branch { name } => name == branch,
                UnprobedHead::Detached { .. } => false,
                UnprobedHead::Unknown => true,
            })
            .count()
    }
}

/// A regular file's contents (the path followed) as git takes a C string
/// from a buffer it read whole and trimmed: the bytes before the first NUL,
/// untrimmed, since trimming the end can't reach past a NUL; or, with no
/// NUL, the whole file less its trailing bytes `trimmed` matches. Read in
/// chunks and stopped at the first NUL, so a file of any size with one
/// early is read as git reads it; one with no NUL in its first `max` bytes
/// is an error, as is an empty one (git refuses an empty `commondir`, and
/// an empty `HEAD` names nothing).
fn read_c_string(path: &Path, max: u64, trimmed: impl Fn(u8) -> bool) -> std::io::Result<Vec<u8>> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    let mut chunk = vec![0; C_STRING_CHUNK];
    loop {
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        if let Some(nul) = chunk[..n].iter().position(|&b| b == 0) {
            bytes.extend_from_slice(&chunk[..nul]);
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("no NUL in its first {max} bytes"),
            ));
        }
    }
    if bytes.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty",
        ));
    }
    let end = trim_end(&bytes, trimmed).len();
    bytes.truncate(end);
    Ok(bytes)
}

/// Whether git's `isspace` holds for `b`: git's own ctype, the ASCII space,
/// tab, and line breaks, not the locale's (no form feed or vertical tab).
const fn is_git_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether `b` is a line break, all git trims from a `commondir` or gitfile.
const fn is_line_break(b: u8) -> bool {
    matches!(b, b'\n' | b'\r')
}

/// The indices of the candidates `cwd` sits deepest in, by path component
/// (`/ws/app` holds `/ws/app/src`, not `/ws/app-wt`); several when they tie,
/// none when it's in none.
fn deepest_containing<'p>(
    cwd: &Path,
    candidates: impl IntoIterator<Item = &'p Path>,
) -> Vec<usize> {
    let within: Vec<(usize, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter(|(_, c)| cwd.starts_with(c))
        .map(|(i, c)| (i, c.components().count()))
        .collect();
    let deepest = within.iter().map(|&(_, depth)| depth).max();
    within
        .into_iter()
        .filter(|&(_, depth)| Some(depth) == deepest)
        .map(|(i, _)| i)
        .collect()
}

/// The indices of the candidates under `root`'s `.claude/worktrees/`
/// (resolved), by path component, at any depth: where Claude Code puts the
/// agent worktrees of a session rooted at `root`, whose subagents' sessions
/// keep the parent's cwd (the module doc's **Claude Code's own worktrees**).
///
/// None when that dir can't be resolved (a symlink loop, a dir the tool
/// can't search): no checkout under it can be either, so each is already
/// its entry's `unresolved`, and held.
fn nested_worktrees<'p>(root: &Path, candidates: impl IntoIterator<Item = &'p Path>) -> Vec<usize> {
    let Ok(dir) = resolve(&root.join(".claude/worktrees")) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .enumerate()
        .filter(|&(_, c)| c.starts_with(&dir) && c != dir)
        .map(|(i, _)| i)
        .collect()
}

/// Where Claude Code roots the worktrees of a session in a checkout of the
/// repo whose common dir is `common` (canonical), when the checkout's
/// worktree link verifies: the dir holding it when it's named `.git` (the
/// primary checkout), else the common dir itself (a bare repo, or a
/// `--separate-git-dir` one).
fn claude_root(common: &Path) -> PathBuf {
    match (common.file_name(), common.parent()) {
        (Some(name), Some(parent)) if name == ".git" => parent.to_owned(),
        _ => common.to_owned(),
    }
}

/// Where resolving a path stopped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unresolved {
    path: String,
    error: String,
}

impl From<Unresolved> for Unavailable {
    fn from(u: Unresolved) -> Self {
        Self::Unreadable {
            path: u.path,
            error: u.error,
        }
    }
}

/// An absolute path resolved as the kernel would — symlinks followed, `.`
/// and `..` applied — component by component, so a component that doesn't
/// exist (a deleted dir, or a name under a file) is taken as written and
/// `..` past it undoes it, while whatever does exist around it still
/// resolves.
///
/// A component that can't be looked up for any other reason — in a dir the
/// tool can't search, a symlink loop (which the kernel cuts short) — fails:
/// where the path leads can't be told, so taking it as written could leave
/// a session unscoped.
fn resolve(path: &Path) -> Result<PathBuf, Unresolved> {
    if let Ok(real) = path.canonicalize() {
        return Ok(real);
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c),
            Component::CurDir => {}
            // `out` is canonical up to any component taken as written, and
            // neither is a symlink: `..` is lexical from here
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                out.push(name);
                match out.canonicalize() {
                    Ok(real) => out = real,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) => {}
                    Err(e) => {
                        return Err(Unresolved {
                            path: out.to_string_lossy().into_owned(),
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(out)
}

/// One entry's checkouts, as busy detection scopes sessions to them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryCheckouts {
    /// Every checkout's path as the probe's facts spell it: the primary's,
    /// each probed worktree's, each unprobed one's.
    pub paths: Vec<String>,
    /// Each checkout's own git dir, canonicalized when it can be, with its
    /// path as `paths` spells it (`RepoFacts::git_dirs`).
    pub git_dirs: Vec<(PathBuf, String)>,
    /// The repo's common dir (`RepoFacts::common_dir`): a git dir no
    /// worktree list names that shares it is `unlisted`.
    pub common_dir: Option<PathBuf>,
    /// Each locked checkout's lock reason, with its path as `paths` spells
    /// it (`RepoFacts::locks`): one Claude Code wrote names the session
    /// working there (`claude_lock`).
    pub locks: Vec<(String, String)>,
}

/// What a worktree lock Claude Code wrote names: its process's pid, and
/// that process's `starttime` when the lock gives one (the module doc's
/// **Claude Code's worktree locks**).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeLock<'a> {
    pid: u64,
    start: Option<&'a str>,
}

impl ClaudeLock<'_> {
    /// Whether it names `session`: the same pid, and the same start time to
    /// the digit when it gives one, as Claude Code compares them.
    fn names(&self, session: &Session) -> bool {
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
fn claude_lock(reason: &str) -> Option<ClaudeLock<'_>> {
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

/// A checkout's lock that Claude Code wrote, by entry and path as the
/// probe's facts spell it.
#[derive(Debug)]
struct CheckoutLock<'a> {
    entry: usize,
    checkout: &'a str,
    lock: ClaudeLock<'a>,
}

/// Scopes live sessions to checkouts: `checkouts[e]` are entry `e`'s.
/// Returns busy detection as the report carries it, and what each entry's
/// classification holds on.
///
/// Every checkout is resolved first, whatever `live` holds: one that can't
/// be is its owners' `unresolved` (each entry it's a checkout of), and
/// takes no part in the prefix scoping.
pub fn scope_sessions(
    live: &LiveSessions,
    checkouts: &[EntryCheckouts],
) -> (Sessions, Vec<EntrySessions>) {
    let mut resolved = Vec::new();
    let mut unresolved: Vec<BTreeMap<String, UnresolvedCheckout>> =
        checkouts.iter().map(|_| BTreeMap::new()).collect();
    let mut known = KnownGitDirs::default();
    let mut locks = Vec::new();
    for (e, entry) in checkouts.iter().enumerate() {
        locks.extend(entry.locks.iter().filter_map(|(checkout, reason)| {
            claude_lock(reason).map(|lock| CheckoutLock {
                entry: e,
                checkout,
                lock,
            })
        }));
        for checkout in &entry.paths {
            match resolve(Path::new(checkout)) {
                Ok(real) => resolved.push(Candidate {
                    entry: e,
                    checkout,
                    real,
                }),
                Err(Unresolved { path, error }) => {
                    unresolved[e].insert(checkout.clone(), UnresolvedCheckout { path, error });
                }
            }
        }
        for (git_dir, checkout) in &entry.git_dirs {
            known
                .owners
                .entry(git_dir)
                .or_default()
                .push((e, checkout.as_str()));
        }
        if let Some(common) = &entry.common_dir {
            let real = common.canonicalize().unwrap_or_else(|_| common.clone());
            known.roots.push(Some(claude_root(&real)));
            known.commons.entry(real).or_default().push(e);
        } else {
            known.roots.push(None);
        }
    }
    let scoped = match live {
        LiveSessions::Known(sessions) => scope_known(sessions, &resolved, &known, &locks),
        LiveSessions::Unavailable(reason) => Err(reason.clone()),
    };
    let (report, mut per_entry) = scoped.unwrap_or_else(|reason| {
        (
            Sessions::Unavailable { reason },
            checkouts
                .iter()
                .map(|_| EntrySessions::unavailable())
                .collect(),
        )
    });
    for (entry, unresolved) in per_entry.iter_mut().zip(unresolved) {
        entry.unresolved = unresolved;
    }
    (report, per_entry)
}

/// A checkout to scope sessions to: whose it is, and its path as the
/// probe's facts spell it and resolved.
#[derive(Debug)]
struct Candidate<'a> {
    entry: usize,
    checkout: &'a str,
    real: PathBuf,
}

/// The git dirs the probe knows, canonical: each checkout's own, by entry
/// and path, and each entry's common dir — and, by entry, where Claude Code
/// roots its agent worktrees (`claude_root`).
#[derive(Debug, Default)]
struct KnownGitDirs<'a> {
    owners: BTreeMap<&'a Path, Vec<(usize, &'a str)>>,
    commons: BTreeMap<PathBuf, Vec<usize>>,
    roots: Vec<Option<PathBuf>>,
}

/// Where the attribution walk places a session.
#[derive(Debug, PartialEq, Eq)]
enum Attributed<'a> {
    /// No git dir the probe knows on the way up.
    Nowhere,
    /// The checkouts whose own git dir it found, by entry and path.
    Checkouts {
        owners: Vec<(usize, &'a str)>,
        /// Whether that git dir is a common dir: the checkouts are primaries.
        primary: bool,
        /// The dir whose `.git` named it, the toplevel Claude Code finds;
        /// `None` for a session inside the git dir itself.
        toplevel: Option<PathBuf>,
    },
    /// A git dir no worktree list names that shares these entries' refs.
    Unlisted {
        entries: Vec<usize>,
        git_dir: PathBuf,
        head: UnprobedHead,
        /// As for `Checkouts`.
        toplevel: Option<PathBuf>,
    },
}

/// `scope_sessions` over sessions the reader vouched for, the checkouts
/// that resolved, the git dirs the probe knows (and each entry's root), and
/// the checkouts' locks Claude Code wrote: fails when a place a session
/// works in can't be resolved.
fn scope_known(
    sessions: &[Session],
    candidates: &[Candidate<'_>],
    known: &KnownGitDirs<'_>,
    locks: &[CheckoutLock<'_>],
) -> Result<(Sessions, Vec<EntrySessions>), Unavailable> {
    let mut per_entry: Vec<EntrySessions> =
        known.roots.iter().map(|_| EntrySessions::idle()).collect();
    let reals = || candidates.iter().map(|c| c.real.as_path());
    let mut unscoped = Vec::new();
    for session in sessions {
        let mut at: Vec<(usize, &str)> = Vec::new();
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut placed = false;
        for place in session.places() {
            let cwd = resolve(Path::new(place))?;
            at.extend(
                deepest_containing(&cwd, reals())
                    .into_iter()
                    .map(|i| (candidates[i].entry, candidates[i].checkout)),
            );
            match attributed(&cwd, known) {
                Attributed::Nowhere => {}
                Attributed::Checkouts {
                    owners,
                    primary,
                    toplevel,
                } => {
                    // Claude Code roots a linked worktree's agent worktrees
                    // at its repo's (below) only when the worktree's link
                    // back verifies, as it does where the worktree list
                    // says it is; a primary's, and a moved one's, at the
                    // toplevel it found
                    let listed = |top: &Path| {
                        owners.iter().all(|&(e, checkout)| {
                            candidates
                                .iter()
                                .any(|c| c.entry == e && c.checkout == checkout && c.real == top)
                        })
                    };
                    roots.extend(toplevel.filter(|top| primary || !listed(top)));
                    at.extend(owners);
                }
                Attributed::Unlisted {
                    entries,
                    git_dir,
                    head,
                    toplevel,
                } => {
                    placed = true;
                    roots.extend(toplevel);
                    let key = git_dir.to_string_lossy().into_owned();
                    for e in entries {
                        roots.extend(known.roots[e].clone());
                        let busy = &mut per_entry[e]
                            .unlisted
                            .entry(key.clone())
                            .or_insert_with(|| UnlistedGitDir {
                                head: head.clone(),
                                busy: Vec::new(),
                            })
                            .busy;
                        if !busy.contains(session) {
                            busy.push(session.clone());
                        }
                    }
                }
            }
        }
        // each entry it's in, its repo's root: where Claude Code puts the
        // agent worktrees of a session anywhere in a checkout of it
        roots.extend(at.iter().filter_map(|&(e, _)| known.roots[e].clone()));
        roots.sort_unstable();
        roots.dedup();
        for root in &roots {
            at.extend(
                nested_worktrees(root, reals())
                    .into_iter()
                    .map(|i| (candidates[i].entry, candidates[i].checkout)),
            );
        }
        // the checkouts Claude Code locked for it, wherever they are
        at.extend(
            locks
                .iter()
                .filter(|l| l.lock.names(session))
                .map(|l| (l.entry, l.checkout)),
        );
        at.sort_unstable();
        at.dedup();
        if at.is_empty() && !placed {
            unscoped.push(session.clone());
        }
        for (e, checkout) in at {
            per_entry[e]
                .busy
                .entry(checkout.to_owned())
                .or_default()
                .push(session.clone());
        }
    }
    Ok((Sessions::Available { unscoped }, per_entry))
}

/// Where a session at `cwd` (resolved) works by git's own lights: the
/// checkouts whose own git dir the nearest `.git` on its walk up names (or
/// the nearest dir that is such a git dir), the unlisted git dir sharing an
/// entry's refs it names, or `Nowhere`. What the walk follows, what it
/// passes over, and why are the module doc's **Attribution**.
fn attributed<'a>(cwd: &Path, known: &KnownGitDirs<'a>) -> Attributed<'a> {
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        // followed, as git's own stat is: a symlinked `.git` counts
        let target = match std::fs::metadata(&dot_git) {
            Ok(m) if m.is_dir() => Some(dot_git),
            Ok(m) if m.is_file() => gitfile_target(&dot_git),
            Ok(_) | Err(_) => None,
        };
        if let Some(found) = target.and_then(|t| known_git_dir(&t, dir, known)) {
            return found;
        }
        // `cwd` is resolved, so each ancestor is canonical as it stands
        if let Some(owners) = known.owners.get(dir) {
            return Attributed::Checkouts {
                owners: owners.clone(),
                primary: known.commons.contains_key(dir),
                toplevel: None,
            };
        }
    }
    Attributed::Nowhere
}

/// What the git dir at `path`, named by `toplevel`'s `.git`, is to the
/// probe: a checkout's own, an unlisted one sharing an entry's refs, or
/// neither (`None`, and when it can't be looked up).
fn known_git_dir<'a>(
    path: &Path,
    toplevel: &Path,
    known: &KnownGitDirs<'a>,
) -> Option<Attributed<'a>> {
    let real = path.canonicalize().ok()?;
    if let Some(owners) = known.owners.get(real.as_path()) {
        return Some(Attributed::Checkouts {
            owners: owners.clone(),
            primary: known.commons.contains_key(&real),
            toplevel: Some(toplevel.to_owned()),
        });
    }
    let common = shared_common_dir(&real, |dir| known.commons.contains_key(dir))?;
    Some(Attributed::Unlisted {
        entries: known.commons.get(&common)?.clone(),
        head: unlisted_head(&real),
        git_dir: real,
        toplevel: Some(toplevel.to_owned()),
    })
}

/// The common dir a git dir the probe doesn't know shares refs with,
/// canonical, when that can be told. With a `commondir` file, the dir it
/// names as git reads it (a C string, trailing line breaks dropped,
/// relative to the git dir; `read_c_string`), and nothing else: git keeps
/// branches there alone. Without one, the first of these `is_common` knows:
/// the dir its `refs` resolves in, a `refs` symlinked into another git dir
/// as `git-new-workdir` makes, or the dir its `refs/heads` resolves two
/// levels up in, a real `refs` with only `heads` symlinked. `None` when a
/// `commondir` can't be read or resolved (git stops with an error there) or
/// has no NUL within the tool's own limit, or neither lookup names a dir
/// `is_common` knows.
fn shared_common_dir(git_dir: &Path, is_common: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    match read_c_string(
        &git_dir.join("commondir"),
        MAX_GIT_C_STRING_BYTES,
        is_line_break,
    ) {
        Ok(bytes) => {
            let named = Path::new(OsStr::from_bytes(&bytes));
            return git_dir.join(named).canonicalize().ok();
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return None,
        Err(_) => {}
    }
    // the dir `path` resolves in, as many levels up as `names`, when those
    // are the names on the way
    let resolved_in = |path: &Path, names: &[&str]| -> Option<PathBuf> {
        let mut dir = path.canonicalize().ok()?;
        for name in names.iter().rev() {
            if dir.file_name() != Some(OsStr::new(name)) {
                return None;
            }
            dir = dir.parent()?.to_owned();
        }
        Some(dir)
    };
    [
        resolved_in(&git_dir.join("refs"), &["refs"]),
        resolved_in(&git_dir.join("refs/heads"), &["refs", "heads"]),
    ]
    .into_iter()
    .flatten()
    .find(|dir| is_common(dir))
}

/// An unlisted git dir's `HEAD`, read as git reads a loose ref: the file a
/// C string with trailing whitespace dropped (`read_c_string`, git's own
/// whitespace), then `ref:` and optional whitespace naming a branch, or a
/// full object id. A symlink (git's oldest form, the link naming the
/// branch), a name that isn't UTF-8, and anything else are `Unknown`.
fn unlisted_head(git_dir: &Path) -> UnprobedHead {
    let head = git_dir.join("HEAD");
    if !std::fs::symlink_metadata(&head).is_ok_and(|m| m.is_file()) {
        return UnprobedHead::Unknown;
    }
    let Ok(bytes) = read_c_string(&head, MAX_GIT_C_STRING_BYTES, is_git_space) else {
        return UnprobedHead::Unknown;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return UnprobedHead::Unknown;
    };
    if let Some(target) = text.strip_prefix("ref:") {
        return target
            .trim_start_matches(|c: char| u8::try_from(c).is_ok_and(is_git_space))
            .strip_prefix("refs/heads/")
            .map_or(UnprobedHead::Unknown, |name| UnprobedHead::Branch {
                name: name.to_owned(),
            });
    }
    if is_object_id(&text) {
        return UnprobedHead::Detached { commit: text };
    }
    UnprobedHead::Unknown
}

/// `bytes` less its trailing bytes `trimmed` matches.
fn trim_end(bytes: &[u8], trimmed: impl Fn(u8) -> bool) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| !trimmed(b))
        .map_or(0, |i| i + 1);
    &bytes[..end]
}

/// A path as git takes one from a buffer: a C string, so up to the first
/// NUL, and raw bytes, UTF-8 or not.
fn c_path(bytes: &[u8]) -> &Path {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Path::new(OsStr::from_bytes(&bytes[..end]))
}

/// The git dir a `.git` file names, as git's `read_gitfile_gently` reads
/// it: a regular file of at most `MAX_GITFILE_BYTES` starting `gitdir: `;
/// trailing line breaks dropped from the whole file, which must leave a
/// byte past the prefix; the path the rest up to the first NUL, raw bytes,
/// relative to the file's dir unless absolute. `None` where git stops with
/// an error instead: the file can't be read, is too large, or isn't a
/// gitfile.
fn gitfile_target(dot_git: &Path) -> Option<PathBuf> {
    const PREFIX: &[u8] = b"gitdir: ";
    let bytes = read_bounded_bytes(dot_git, MAX_GITFILE_BYTES).ok()?;
    if !bytes.starts_with(PREFIX) {
        return None;
    }
    let trimmed = trim_end(&bytes, is_line_break);
    if trimmed.len() <= PREFIX.len() {
        return None;
    }
    let named = c_path(&trimmed[PREFIX.len()..]);
    Some(dot_git.parent().unwrap_or(dot_git).join(named))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionSource;

    /// Entries' checkouts by path alone, with no git dirs.
    fn at_paths(entries: Vec<Vec<String>>) -> Vec<EntryCheckouts> {
        entries
            .into_iter()
            .map(|paths| EntryCheckouts {
                paths,
                ..EntryCheckouts::default()
            })
            .collect()
    }

    #[test]
    fn paths_resolve_as_the_kernel_would_where_they_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let real = base.join("real");
        std::fs::create_dir_all(real.join("app/src")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::os::unix::fs::symlink(real.join("app"), real.join("app-link")).unwrap();
        let at = |p: &Path| resolve(p).unwrap();
        assert_eq!(at(&link.join("app/src")), real.join("app/src"));
        // a deleted dir under a symlink: the rest still resolves
        assert_eq!(at(&link.join("app/gone/x")), real.join("app/gone/x"));
        // `..` past a dir that doesn't exist undoes it, not the symlink
        assert_eq!(at(&link.join("gone/../app")), real.join("app"));
        assert_eq!(at(&link.join("app/../other")), real.join("other"));
        assert_eq!(at(&link.join("gone/./../app-link/y")), real.join("app/y"));
        // `..` out of a symlinked dir goes to its target's parent
        assert_eq!(at(&real.join("app-link/../gone")), real.join("gone"));
        assert_eq!(
            at(Path::new("/nonexistent-ws/a/../b")),
            Path::new("/nonexistent-ws/b")
        );
        // a name under a file: there's nothing there either
        std::fs::write(real.join("file"), "").unwrap();
        assert_eq!(at(&link.join("file/x/../y")), real.join("file/y"));
    }

    /// Whether `resolve` failed at `path`.
    fn fails_at(got: Result<PathBuf, Unresolved>, path: &Path) -> bool {
        matches!(got, Err(Unresolved { path: p, .. }) if p == path.to_string_lossy())
    }

    #[test]
    fn a_symlink_loop_fails_to_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(base.join("b"), base.join("a")).unwrap();
        std::os::unix::fs::symlink(base.join("a"), base.join("b")).unwrap();
        std::os::unix::fs::symlink(base.join("self"), base.join("self")).unwrap();
        for (path, at) in [
            (base.join("a/app/src"), base.join("a")),
            (base.join("self"), base.join("self")),
            (base.join("self/../x"), base.join("self")),
        ] {
            let got = resolve(&path);
            assert!(fails_at(got.clone(), &at), "{}: {got:?}", path.display());
        }
    }

    /// Restores a dir's mode when dropped, so a failed assertion doesn't
    /// leave a tempdir that can't be removed.
    struct Unlock<'a>(&'a Path);

    impl Drop for Unlock<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn a_dir_that_cannot_be_searched_fails_to_resolve() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let locked = base.join("locked");
        std::fs::create_dir_all(locked.join("app/src")).unwrap();
        let cwd = locked.join("app/src");
        assert_eq!(resolve(&cwd).unwrap(), cwd);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _unlock = Unlock(&locked);
        // root searches it anyway, so there's nothing to test
        match std::fs::symlink_metadata(locked.join("app")) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
            other => {
                eprintln!(
                    "skipped: {} is searchable at mode 0 ({other:?})",
                    locked.display()
                );
                return;
            }
        }
        let got = resolve(&cwd);
        assert!(fails_at(got.clone(), &locked.join("app")), "{got:?}");
        // the dir itself resolves: its parent can be searched
        assert_eq!(resolve(&locked).unwrap(), locked);
    }

    #[test]
    fn scoping_fails_closed_on_a_cwd_it_cannot_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(base.join("app")).unwrap();
        std::os::unix::fs::symlink(base.join("loop"), base.join("loop")).unwrap();
        let s = |cwd: &Path| {
            Session::at(
                1,
                0,
                cwd.to_string_lossy().into_owned(),
                SessionSource::SessionFile,
            )
        };
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let checkouts = at_paths(vec![vec![path(&base.join("app"))]]);
        let live = LiveSessions::Known(vec![s(&base.join("app")), s(&base.join("loop/x"))]);
        let (report, per_entry) = scope_sessions(&live, &checkouts);
        assert_eq!(
            report,
            Sessions::Unavailable {
                reason: Unavailable::Unreadable {
                    path: path(&base.join("loop")),
                    error: "Too many levels of symbolic links (os error 40)".into(),
                }
            }
        );
        assert_eq!(per_entry, [EntrySessions::unavailable()]);
    }

    #[test]
    fn a_checkout_it_cannot_resolve_is_unresolved_for_each_entry_it_is_of() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for dir in ["app", "lib", "shared"] {
            std::fs::create_dir(base.join(dir)).unwrap();
        }
        std::os::unix::fs::symlink(base.join("loop"), base.join("loop")).unwrap();
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let s = |pid, cwd: &Path| Session::at(pid, 0, path(cwd), SessionSource::SessionFile);
        // `app`'s worktree in the loop, beside a checkout it shares with
        // `shared`, whose own worktree is the same loop
        let looped = path(&base.join("loop/wt"));
        let checkouts = at_paths(vec![
            vec![path(&base.join("app")), looped.clone()],
            vec![path(&base.join("lib"))],
            vec![path(&base.join("shared")), looped.clone()],
        ]);
        let unresolved = BTreeMap::from([(
            looped,
            UnresolvedCheckout {
                path: path(&base.join("loop")),
                error: "Too many levels of symbolic links (os error 40)".into(),
            },
        )]);
        let held = |per_entry: &[EntrySessions]| {
            assert_eq!(per_entry[0].unresolved, unresolved);
            assert!(per_entry[1].unresolved.is_empty());
            assert_eq!(per_entry[2].unresolved, unresolved);
        };
        // with no session live, and with some: the same checkouts held
        let (report, per_entry) = scope_sessions(&LiveSessions::Known(vec![]), &checkouts);
        assert_eq!(report, Sessions::Available { unscoped: vec![] });
        held(&per_entry);
        assert!(
            per_entry
                .iter()
                .all(|e| e.detection == Detection::Available)
        );
        let in_lib = s(2, &base.join("lib/src"));
        let at_base = s(3, &base);
        let live = LiveSessions::Known(vec![in_lib.clone(), at_base.clone()]);
        let (report, per_entry) = scope_sessions(&live, &checkouts);
        assert_eq!(
            report,
            Sessions::Available {
                unscoped: vec![at_base]
            }
        );
        held(&per_entry);
        assert_eq!(per_entry[1].at(&path(&base.join("lib"))), [in_lib]);
        assert!(per_entry[0].busy.is_empty() && per_entry[2].busy.is_empty());
        // and with detection unavailable
        let (_, per_entry) = scope_sessions(
            &LiveSessions::Unavailable(Unavailable::HomeUnknown),
            &checkouts,
        );
        held(&per_entry);
    }

    #[test]
    fn a_gitfile_is_read_as_git_reads_it() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let dot_git = base.join(".git");
        let target = |content: &[u8]| {
            std::fs::write(&dot_git, content).unwrap();
            gitfile_target(&dot_git)
        };
        // relative to the file's dir, and absolute as written; trailing line
        // breaks dropped, nothing else
        assert_eq!(target(b"gitdir: ../g\n"), Some(base.join("../g")));
        assert_eq!(target(b"gitdir: /g/w\r\n"), Some(PathBuf::from("/g/w")));
        assert_eq!(target(b"gitdir: g \n"), Some(base.join("g ")));
        assert_eq!(target(b"gitdir: g\n\n\r\n"), Some(base.join("g")));
        // a C string: cut at the first NUL, after the line breaks are trimmed
        // from the end of the whole file
        assert_eq!(target(b"gitdir: /g\0junk\n"), Some(PathBuf::from("/g")));
        assert_eq!(target(b"gitdir: /g\n\0\n"), Some(PathBuf::from("/g\n")));
        // nothing before the NUL is the file's own dir, as git joins it
        assert_eq!(target(b"gitdir: \0x"), Some(base.join("")));
        // raw bytes, UTF-8 or not
        assert_eq!(
            target(b"gitdir: /g\xff\n"),
            Some(PathBuf::from(OsStr::from_bytes(b"/g\xff")))
        );
        // anything else isn't a gitfile: git stops with an error
        let not: [&[u8]; 6] = [
            b"",
            b"gitdir: \n",
            b"gitdir:g\n",
            b"x\ngitdir: g\n",
            b" gitdir: g",
            b"gitdir\0: g",
        ];
        for bad in not {
            assert_eq!(target(bad), None, "{bad:?}");
        }
        // git's size limit, padding included
        let max = usize::try_from(MAX_GITFILE_BYTES).unwrap();
        let mut at_limit = b"gitdir: /g".to_vec();
        at_limit.resize(max, b'\n');
        assert_eq!(target(&at_limit), Some(PathBuf::from("/g")));
        at_limit.push(b'\n');
        assert_eq!(target(&at_limit), None);
    }

    #[test]
    fn a_commondir_is_read_as_git_reads_it() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let git_dir = base.join("hand");
        let common = base.join("common");
        std::fs::create_dir_all(git_dir.join("refs")).unwrap();
        std::fs::create_dir(&common).unwrap();
        let any = |_: &Path| true;
        let shared = |content: &[u8]| {
            std::fs::write(git_dir.join("commondir"), content).unwrap();
            shared_common_dir(&git_dir, any)
        };
        assert_eq!(shared(b"../common\n"), Some(common.clone()));
        assert_eq!(shared(b"../common\0junk\r\n"), Some(common.clone()));
        let absolute = format!("{}\n", common.display());
        assert_eq!(shared(absolute.as_bytes()), Some(common.clone()));
        // a C string: trimming the end can't reach past a NUL, and nothing
        // before one is the git dir itself
        assert_eq!(shared(b"../common\n\0\n"), None);
        assert_eq!(shared(b"\0../common"), Some(git_dir.clone()));
        assert_eq!(shared(b"\n"), Some(git_dir.clone()));
        // git reads it whole however large, so one with a NUL in reach is
        // read past any size limit
        let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
        let mut large = b"../common\0".to_vec();
        large.resize(max + C_STRING_CHUNK * 2, b'x');
        assert_eq!(shared(&large), Some(common.clone()));
        // the tool's own limit: no NUL in reach, passed over
        let mut padded = b"../common".to_vec();
        padded.resize(max + 1, b'\n');
        assert_eq!(shared(&padded), None);
        // one git can't use stops git, and no `refs` is looked at
        assert_eq!(shared(b""), None);
        assert_eq!(shared(b"../nowhere\n"), None);
        // without one, the dir its `refs` resolves in
        std::fs::remove_file(git_dir.join("commondir")).unwrap();
        assert_eq!(shared_common_dir(&git_dir, any), Some(git_dir.clone()));
        std::fs::remove_dir(git_dir.join("refs")).unwrap();
        std::fs::create_dir_all(common.join("refs/heads")).unwrap();
        std::os::unix::fs::symlink("../common/refs", git_dir.join("refs")).unwrap();
        assert_eq!(shared_common_dir(&git_dir, any), Some(common.clone()));
        // a `refs` of another name is no git dir's
        std::fs::remove_file(git_dir.join("refs")).unwrap();
        std::fs::create_dir(common.join("other")).unwrap();
        std::os::unix::fs::symlink("../common/other", git_dir.join("refs")).unwrap();
        assert_eq!(shared_common_dir(&git_dir, any), None);
        // or the dir `refs/heads` resolves in, when only `heads` is linked
        // and `refs` names no common dir known
        std::fs::remove_file(git_dir.join("refs")).unwrap();
        std::fs::create_dir(git_dir.join("refs")).unwrap();
        std::os::unix::fs::symlink("../../common/refs/heads", git_dir.join("refs/heads")).unwrap();
        assert_eq!(shared_common_dir(&git_dir, any), Some(git_dir.clone()));
        let known = |dir: &Path| dir == common;
        assert_eq!(shared_common_dir(&git_dir, known), Some(common.clone()));
        let unknown = |_: &Path| false;
        assert_eq!(shared_common_dir(&git_dir, unknown), None);
        // a `heads` of another name, or under no `refs`, is no git dir's
        std::fs::remove_file(git_dir.join("refs/heads")).unwrap();
        std::os::unix::fs::symlink("../../common/other", git_dir.join("refs/heads")).unwrap();
        assert_eq!(shared_common_dir(&git_dir, known), None);
    }

    #[test]
    fn an_unlisted_head_is_read_as_git_reads_a_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let head = |content: &[u8]| {
            std::fs::write(git_dir.join("HEAD"), content).unwrap();
            unlisted_head(git_dir)
        };
        let on = |name: &str| UnprobedHead::Branch { name: name.into() };
        assert_eq!(head(b"ref: refs/heads/main\n"), on("main"));
        assert_eq!(head(b"ref:refs/heads/a/b \n\n"), on("a/b"));
        assert_eq!(head(b"ref:\trefs/heads/x"), on("x"));
        // a C string: cut at the first NUL, and trimming the end can't reach
        // past it
        assert_eq!(head(b"ref: refs/heads/other\0junk\n"), on("other"));
        assert_eq!(head(b"ref: refs/heads/x \0\n"), on("x "));
        let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
        let mut large = b"ref: refs/heads/big\0".to_vec();
        large.resize(max + C_STRING_CHUNK * 2, b'x');
        assert_eq!(head(&large), on("big"));
        // git's own whitespace, not the locale's
        assert_eq!(head(b"ref: refs/heads/x\x0c\n"), on("x\x0c"));
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            head(format!("{id}\n").as_bytes()),
            UnprobedHead::Detached { commit: id.into() }
        );
        assert_eq!(
            head(format!("{id}\0junk").as_bytes()),
            UnprobedHead::Detached { commit: id.into() }
        );
        let unknown: [&[u8]; 7] = [
            b"ref: refs/tags/v1\n",
            b"",
            b"\0ref: refs/heads/main\n",
            b"garbage\n",
            b"ref: main\n",
            b"ref:\x0crefs/heads/x\n",
            b"ref: refs/heads/\xff\n",
        ];
        for bad in unknown {
            assert_eq!(head(bad), UnprobedHead::Unknown, "{bad:?}");
        }
        // the tool's own limit: no NUL in reach
        let mut padded = b"ref: refs/heads/main".to_vec();
        padded.resize(max + 1, b'\n');
        assert_eq!(head(&padded), UnprobedHead::Unknown);
        // a symlink names its branch by the link: not read here
        std::fs::remove_file(git_dir.join("HEAD")).unwrap();
        std::fs::write(git_dir.join("main"), format!("{id}\n")).unwrap();
        std::os::unix::fs::symlink("main", git_dir.join("HEAD")).unwrap();
        assert_eq!(unlisted_head(git_dir), UnprobedHead::Unknown);
    }

    #[test]
    fn the_deepest_containing_checkout_by_component() {
        let c: Vec<PathBuf> = [
            "/ws/app",
            "/ws/app/.claude/worktrees/feat",
            "/ws/app-wt",
            "/ws/b",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let at = |cwd: &str| deepest_containing(Path::new(cwd), c.iter().map(PathBuf::as_path));
        assert_eq!(at("/ws/app"), [0]);
        assert_eq!(at("/ws/app/src"), [0]);
        assert_eq!(at("/ws/app/.claude/worktrees/feat/src"), [1]);
        assert_eq!(at("/ws/app/.claude/worktrees/feature"), [0]);
        assert_eq!(at("/ws/app-wt/x"), [2]);
        assert!(at("/ws").is_empty());
        assert!(at("/elsewhere").is_empty());
        // one path that's two entries' checkouts: both
        let tie: Vec<PathBuf> = ["/ws/a", "/ws/b", "/ws/b"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            deepest_containing(Path::new("/ws/b/x"), tie.iter().map(PathBuf::as_path)),
            [1, 2]
        );
    }

    #[test]
    fn scoping_marks_checkouts_and_leaves_the_rest_unscoped() {
        let s = |pid, cwd: &str| Session::at(pid, 0, cwd.into(), SessionSource::SessionFile);
        let live = LiveSessions::Known(vec![
            s(1, "/nonexistent-ws/app/src"),
            s(2, "/nonexistent-ws"),
            s(3, "/nonexistent-ws/b"),
        ]);
        let checkouts = at_paths(vec![
            vec!["/nonexistent-ws/app".to_owned()],
            vec!["/nonexistent-ws/b".to_owned()],
        ]);
        let (report, per_entry) = scope_sessions(&live, &checkouts);
        assert_eq!(
            report,
            Sessions::Available {
                unscoped: vec![s(2, "/nonexistent-ws")]
            }
        );
        assert_eq!(
            per_entry[0].at("/nonexistent-ws/app"),
            [s(1, "/nonexistent-ws/app/src")]
        );
        assert_eq!(
            per_entry[1].at("/nonexistent-ws/b"),
            [s(3, "/nonexistent-ws/b")]
        );
        assert_eq!(per_entry[0].detection, Detection::Available);

        let reason = Unavailable::HomeUnknown;
        let (report, per_entry) =
            scope_sessions(&LiveSessions::Unavailable(reason.clone()), &checkouts);
        assert_eq!(report, Sessions::Unavailable { reason });
        assert!(
            per_entry
                .iter()
                .all(|e| e.detection == Detection::Unavailable)
        );
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
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
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

    #[test]
    fn scoping_marks_the_checkouts_whose_lock_names_a_session() {
        let s = |pid, start| {
            Session::at(
                pid,
                start,
                "/nonexistent-ws".into(),
                SessionSource::SessionFile,
            )
        };
        let live = LiveSessions::Known(vec![s(1, 10), s(2, 20)]);
        let checkout = |paths: &[&str], locks: &[(&str, &str)]| EntryCheckouts {
            paths: paths.iter().map(|&p| p.to_owned()).collect(),
            locks: locks
                .iter()
                .map(|&(p, r)| (p.to_owned(), r.to_owned()))
                .collect(),
            ..EntryCheckouts::default()
        };
        let checkouts = vec![
            checkout(
                &[
                    "/nonexistent-ws/app",
                    "/nonexistent-ws/app-a",
                    "/nonexistent-ws/app-b",
                ],
                &[
                    ("/nonexistent-ws/app-a", "claude agent a (pid 1 start 10)"),
                    ("/nonexistent-ws/app-b", "claude agent b (pid 2 start 21)"),
                ],
            ),
            // a primary locked for a session, and a lock by hand
            checkout(
                &["/nonexistent-ws/lib", "/nonexistent-ws/lib-c"],
                &[
                    ("/nonexistent-ws/lib", "claude session lib (pid 2)"),
                    ("/nonexistent-ws/lib-c", "pid 1"),
                ],
            ),
        ];
        let (report, per_entry) = scope_sessions(&live, &checkouts);
        assert_eq!(report, Sessions::Available { unscoped: vec![] });
        let busy = |e: &EntrySessions| {
            e.busy
                .iter()
                .map(|(path, sessions)| {
                    let pids: Vec<u32> = sessions.iter().map(|s| s.pid).collect();
                    (path.clone(), pids)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            busy(&per_entry[0]),
            [("/nonexistent-ws/app-a".to_owned(), vec![1])]
        );
        assert_eq!(
            busy(&per_entry[1]),
            [("/nonexistent-ws/lib".to_owned(), vec![2])]
        );
    }
}
