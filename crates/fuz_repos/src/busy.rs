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
//! Code roots only its linked worktrees' sessions at. Those checkouts are
//! busy with it for the holds, but it doesn't work in them itself, so they
//! aren't in its entry's `working`: a subagent's worktree there is known
//! as its own by the lock Claude Code puts on it (below).
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
//! Its `commondir` and `HEAD` are read as git reads them (`gitdir`); one
//! past the tool's own read limit is passed over (a `commondir`, where git
//! might follow it) or unknown (a `HEAD`, which holds every branch).
//!
//! Everything else at a `.git` is passed over, and the walk goes on. Git
//! passes over a `.git` it can't look up (a dir it can't search, a symlink
//! loop) or that's no dir or regular file, and stops with an error at a
//! gitfile it can't read, over its 1 MiB limit, or not starting `gitdir: `,
//! or naming no git dir — so no session commits through any of them. A
//! gitfile is read exactly as git reads it (`gitdir::read_gitfile`), so
//! every gitfile git follows is followed. A git dir the probe doesn't know
//! is passed over as the prefix scoping passes over repos it doesn't know
//! (a session in a repo nested in a moved worktree is still in the
//! worktree's files).
//! Walking on only attributes more, and a `.git` never makes detection
//! unavailable. The walk is one stat per level, a bounded read of a `.git`
//! file, and a few lookups in a git dir the probe doesn't know.
//!
//! **Claude Code's worktree locks** (`sessions::claude_lock`): Claude Code
//! locks each worktree it creates or resumes by name (`EnterWorktree`, a
//! subagent's), and one a background session adopts as it starts, with
//! the reason `claude <agent|session> <name> (pid <pid> start <start>)`:
//! its own process's pid and `starttime` (field 22 of `/proc/<pid>/stat`,
//! as a session file's `procStart` records it), ` start <start>` left out
//! where it has none. A checkout whose lock names a live session is busy
//! with it, wherever the session's places are: probed or unprobed (a missing
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
use std::path::{Component, Path, PathBuf};

use crate::gitdir::{read_commondir, read_gitfile, read_head};
use crate::report::Sessions;
use crate::sessions::{ClaudeLock, LiveSessions, Session, Unavailable, claude_lock};
use crate::state::UnprobedHead;

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
    /// The sessions `busy` holds that work in the checkout itself, keyed
    /// like it: placed there by a place of theirs (its path, or the git dir
    /// found from it) or by the lock Claude Code wrote naming them — `busy`
    /// less what the agent-worktrees rule alone adds (a session elsewhere
    /// in the repo, for the subagent worktrees it may have under
    /// `.claude/worktrees/`). What a checkout's own session is told
    /// (`status --brief`); holds go by `busy`.
    pub working: BTreeMap<String, Vec<Session>>,
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
            working: BTreeMap::new(),
            unresolved: BTreeMap::new(),
            unlisted: BTreeMap::new(),
        }
    }

    /// The sessions in the checkout at `path`.
    pub fn at(&self, path: &str) -> &[Session] {
        self.busy.get(path).map_or(&[], Vec::as_slice)
    }

    /// The sessions working in the checkout at `path` itself (`working`).
    pub fn working_at(&self, path: &str) -> &[Session] {
        self.working.get(path).map_or(&[], Vec::as_slice)
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

/// The live sessions with a place at or under `path`, a missing entry's
/// dir, where its clone would land.
///
/// A session whose dir was deleted from under it keeps its recorded cwd
/// there. Compared by path
/// component, both sides resolved (`resolve`, which takes the missing
/// components as written), and as written besides, so a place that can't
/// be resolved still counts when its text is under `path`.
///
/// None when detection is unavailable: a missing dir holds no work to
/// lose, so a clone doesn't wait on sessions no one can vouch for (the
/// `classify_missing` doc says what holds a clone).
pub fn sessions_under(live: &LiveSessions, path: &Path) -> Vec<Session> {
    let LiveSessions::Known(sessions) = live else {
        return Vec::new();
    };
    let real = resolve(path).ok();
    sessions
        .iter()
        .filter(|s| {
            s.places().any(|place| {
                let place = Path::new(place);
                place.starts_with(path)
                    || real
                        .as_ref()
                        .is_some_and(|real| resolve(place).is_ok_and(|p| p.starts_with(real)))
            })
        })
        .cloned()
        .collect()
}

/// Whether `a` and `b` name one path once each is resolved as the kernel
/// would (`resolve`: missing components taken as written); `false` when
/// either can't be.
pub fn same_path(a: &Path, b: &Path) -> bool {
    matches!((resolve(a), resolve(b)), (Ok(a), Ok(b)) if a == b)
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
        // the checkouts Claude Code locked for it, wherever they are
        at.extend(
            locks
                .iter()
                .filter(|l| l.lock.names(session))
                .map(|l| (l.entry, l.checkout)),
        );
        // where it works itself; the roots' agent worktrees are busy
        // besides, for subagents it may have there
        let mut working = at.clone();
        working.sort_unstable();
        working.dedup();
        for root in &roots {
            at.extend(
                nested_worktrees(root, reals())
                    .into_iter()
                    .map(|i| (candidates[i].entry, candidates[i].checkout)),
            );
        }
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
        for (e, checkout) in working {
            per_entry[e]
                .working
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
            Ok(m) if m.is_file() => read_gitfile(&dot_git).ok(),
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
        head: read_head(&real),
        git_dir: real,
        toplevel: Some(toplevel.to_owned()),
    })
}

/// The common dir a git dir the probe doesn't know shares refs with,
/// canonical, when that can be told. With a `commondir` file, the dir it
/// names as git reads it (`read_commondir`), and nothing else: git keeps
/// branches there alone. Without one, the first of these `is_common` knows:
/// the dir its `refs` resolves in, a `refs` symlinked into another git dir
/// as `git-new-workdir` makes, or the dir its `refs/heads` resolves two
/// levels up in, a real `refs` with only `heads` symlinked. `None` when a
/// `commondir` can't be read or resolved (git stops with an error there) or
/// has no NUL within the tool's own limit, or neither lookup names a dir
/// `is_common` knows.
fn shared_common_dir(git_dir: &Path, is_common: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    match read_commondir(git_dir) {
        Ok(Some(named)) => return named.canonicalize().ok(),
        Ok(None) => {}
        Err(_) => return None,
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

#[cfg(test)]
mod tests;
