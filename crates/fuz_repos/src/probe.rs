//! The per-entry probe: the git calls and file reads behind `classify`.
//!
//! Nothing here writes a working tree, an index, or a
//! local branch; only the optional fetch touches the network, and it writes
//! remote-tracking refs alone, whatever the repo's config asks (`FETCH_ARGS`;
//! a refspec no flag can confine — origin's writing outside
//! `refs/remotes/origin/`, or another remote's writing inside it — isn't
//! fetched).
//!
//! The files it reads itself that git reads too — a worktree's `.git`, a
//! worktree git dir's `HEAD` and `gitdir` — are read as git reads them
//! (`gitdir`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::gitdir::{dot_git_target, read_head, read_worktree_gitdir};
use crate::porcelain::{
    self, ConfigFacts, RefFacts, StatusFacts, Track, WorktreeHead, WorktreeRecord,
};
use crate::registry::Entry;
use crate::regular_file::read_regular;
use crate::remote::{RefspecContext, RemoteFailure};
use crate::state::{
    Checkout, GitDirHolds, Head, InProgressOp, Layout, UnprobedHead, UnprobedWhy, UnprobedWorktree,
};

/// What the probe needs from its caller.
#[derive(Debug, Clone, Copy)]
pub struct ProbeContext<'a> {
    pub git: &'a Git,
    pub root: &'a Path,
    /// Every registry entry's dir, the whole registry's whatever the
    /// targets: a worktree of the probed repo at one is that entry's
    /// checkout.
    pub registry_dirs: &'a RegistryDirs,
    /// Fetch owned, non-pinned entries from `origin` before probing.
    pub fetch: bool,
}

/// Every registry entry's dir under the workspace root, canonicalized.
///
/// Only those that exist. Two entries can share a repo, one a linked
/// worktree of the other, and a worktree at another entry's dir is never
/// advised away with a branch.
#[derive(Debug, Clone, Default)]
pub struct RegistryDirs(HashSet<PathBuf>);

impl RegistryDirs {
    /// `entries` must be the whole registry.
    pub fn new(root: &Path, entries: &[Entry]) -> Self {
        Self(
            entries
                .iter()
                .filter_map(|e| canonical(&root.join(&e.dir)))
                .collect(),
        )
    }

    /// Whether `path`, canonicalized, is a registry entry's dir; `false` when
    /// it can't be canonicalized.
    pub fn contains(&self, path: &Path) -> bool {
        canonical(path).is_some_and(|p| self.0.contains(&p))
    }
}

/// One entry's probe, with its timings.
#[derive(Debug)]
pub struct ProbeRun {
    pub probed: Probed,
    /// `None` when no fetch was attempted; `Some(Err)` says why it failed.
    pub fetch: Option<Result<(), RemoteFailure>>,
    pub fetch_time: Duration,
    pub probe_time: Duration,
}

/// What the probe found.
#[derive(Debug)]
pub enum Probed {
    Missing,
    /// The dir exists but holds no repo; `detail` says why.
    NotARepo {
        detail: String,
    },
    Present(Box<RepoFacts>),
    /// The dir is a repo, but a later call failed. `layout` is recorded
    /// once the config step (and the fetch, when asked) has run, before any
    /// call that reads objects — a partial clone's filter says why a call
    /// may have needed an object the probe never fetches.
    Failed {
        error: String,
        layout: Option<Layout>,
    },
}

/// The facts of a present repo.
#[derive(Debug, Clone)]
pub struct RepoFacts {
    /// The primary checkout's path.
    pub path: String,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub config: ConfigFacts,
    pub status: StatusFacts,
    /// The primary checkout's operation in progress.
    pub in_progress: Option<InProgressOp>,
    /// Whether the primary is itself a linked worktree (its git dir isn't the
    /// common dir).
    pub primary_linked: bool,
    /// Whether the primary is a locked linked worktree.
    pub primary_locked: bool,
    /// Each locked checkout's lock reason as git lists it (empty when none
    /// was given), with its path as `path`, `worktrees`, and `unprobed`
    /// spell it: the primary's when it's a locked linked worktree. A
    /// worktree git doesn't list has none here, its reason unread. Busy
    /// detection reads the ones Claude Code writes, which name the session
    /// working there.
    pub locks: Vec<(String, String)>,
    /// The repo's other worktrees probed — linked ones, and the main one
    /// when the primary is linked — in `git worktree list` order.
    pub worktrees: Vec<Checkout>,
    /// The paths of `worktrees` that are registry entries' dirs: another
    /// entry's checkout, never removable with a branch of this one.
    pub registry_worktrees: HashSet<String>,
    /// The worktrees that couldn't be probed: listed but gone or failing,
    /// or unlisted.
    pub unprobed: Vec<UnprobedWorktree>,
    /// Git dirs whose in-progress markers couldn't be read — a worktree's,
    /// or `<commondir>/worktrees/` itself — so an operation there is
    /// unknowable.
    pub unreadable: Vec<String>,
    /// The first worktree git dir whose `gitdir` names its worktree by a
    /// relative path. Git 2.48+ resolves one against the git dir, older gits
    /// against the cwd, so every worktree path of the repo is uncertain.
    pub relative_gitdir: Option<PathBuf>,
    /// Each checkout's own git dir, canonicalized when it can be, with the
    /// checkout's path as `path`, `worktrees`, and `unprobed` spell it: the
    /// primary's, and every other worktree's the probe found (not one git
    /// lists that no admin dir matches). Busy detection attributes a live
    /// session to the checkout whose git dir its `.git` names.
    pub git_dirs: Vec<(PathBuf, String)>,
    /// The main worktree's path as git lists it, when the repo is bare. It
    /// has no files, so it's never probed, yet `%(worktreepath)` names it
    /// for the branch its HEAD is on.
    pub bare_main: Option<String>,
    pub branches: Vec<BranchFacts>,
    pub layout: Layout,
    /// The newest `FETCH_HEAD` mtime across the repo's worktrees (each keeps
    /// its own), in unix seconds; `None` when none holds a fetch's record
    /// (`newest_fetch`): never fetched, or the last fetch failed or found an
    /// empty remote.
    pub fetched_at: Option<u64>,
}

/// A local branch's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchFacts {
    pub branch: RefFacts,
    /// Commits on no remote-tracking ref, minus shallow roots; counted only
    /// where `could_carry_local_work` says so, else zero.
    pub unique_commits: u32,
    /// In a shallow clone, whether the branch's unique commits sit on the
    /// fetched tip (its upstream is an ancestor); false elsewhere.
    pub on_fetched_tip: bool,
}

/// Whether a branch might hold commits on no remote: anything but a branch
/// level with, or strictly behind, a resolved upstream.
pub const fn could_carry_local_work(r: &RefFacts) -> bool {
    !matches!(
        (&r.upstream_ref, r.track),
        (Some(_), Track::Even | Track::Behind(_))
    )
}

/// Whether `--fetch` fetches this entry: owned and not pinned — a pin is
/// never fetched, whatever branch it's on.
pub const fn fetches(entry: &Entry) -> bool {
    entry.writable && !entry.pinned
}

/// Probes one entry.
pub fn probe(entry: &Entry, cx: ProbeContext<'_>) -> ProbeRun {
    let start = Instant::now();
    let mut early = Recorded::default();
    let probed =
        probe_present(entry, &cx.root.join(&entry.dir), cx, &mut early).unwrap_or_else(|error| {
            Probed::Failed {
                error,
                layout: early.layout.take(),
            }
        });
    ProbeRun {
        probed,
        fetch: early.fetch,
        fetch_time: early.fetch_time,
        probe_time: start.elapsed().saturating_sub(early.fetch_time),
    }
}

/// What the probe records as it goes, kept even when a later step fails:
/// the fetch's outcome and time, and the layout once the config step ran.
#[derive(Debug, Default)]
struct Recorded {
    fetch: Option<Result<(), RemoteFailure>>,
    fetch_time: Duration,
    layout: Option<Layout>,
}

fn probe_present(
    entry: &Entry,
    dir: &Path,
    cx: ProbeContext<'_>,
    early: &mut Recorded,
) -> Result<Probed, String> {
    // 1. presence
    if !dir.exists() {
        return Ok(Probed::Missing);
    }
    let path = dir
        .to_str()
        .ok_or_else(|| format!("non-UTF-8 path {}", dir.display()))?
        .to_owned();
    let local = CallOptions {
        ceiling: Some(cx.root),
        network: None,
    };
    let dirs = match cx.git.output_string(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
        local,
    ) {
        Ok(out) => out,
        Err(GitError::Failed { stderr, .. }) => {
            return Ok(Probed::NotARepo {
                detail: not_a_repo_detail(dir, &stderr),
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut lines = dirs.lines();
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err(format!("rev-parse: unexpected output `{dirs}`"));
    };
    let git_dir = PathBuf::from(git_dir);
    let common_dir = PathBuf::from(common_dir);

    // 4. config, first: fetch needs to know whether SSH is configured
    // the repo's own config file, which the advised `git remote` and `git
    // config` commands edit; git prints its path relative to `dir` from the
    // main checkout, absolute from a linked worktree
    let repo_file = canonical(&common_dir.join("config"));
    let is_repo_file = |path: &str| repo_file.is_some() && canonical(&dir.join(path)) == repo_file;
    let config = match cx.git.run(
        dir,
        &[
            "config",
            "-z",
            "--show-scope",
            "--show-origin",
            "--get-regexp",
            porcelain::CONFIG_PATTERN,
        ],
        local,
    ) {
        // exit 1 is "no matching keys"
        Ok(out) if out.status.success() || out.status.code() == Some(1) => {
            ConfigFacts::parse(&out.stdout, is_repo_file)?
        }
        Ok(out) => return Err(format!("config failed: {}", out.stderr.trim())),
        Err(e) => return Err(e.to_string()),
    };
    let shallow_roots = read_shallow_roots(&common_dir);

    // no `origin` URL, nothing to fetch from — origin drift reports it
    if cx.fetch && fetches(entry) && config.origin_url().is_some() {
        let start = Instant::now();
        let mut args = FETCH_ARGS.to_vec();
        if !shallow_roots.is_empty() {
            args.extend(["--depth", "1"]);
        }
        args.push("origin");
        let net = CallOptions {
            ceiling: Some(cx.root),
            network: Some(NetworkOptions {
                batch_ssh: !config.ssh_command && !cx.git.env_configures_ssh(),
            }),
        };
        // a refspec writing outside `refs/remotes/origin/`, or another
        // remote's writing inside it, where no flag reaches: not fetched at
        // all
        let refused = config
            .origin_fetch
            .iter()
            .find(|v| refspec_writes_outside_origin(&v.value))
            .map(|v| RemoteFailure::RefspecOutsideOrigin {
                refspec: v.value.clone(),
            })
            .or_else(|| {
                config
                    .other_fetch
                    .iter()
                    .find(|r| refspec_writes_into_origin(&r.refspec))
                    .map(|r| RemoteFailure::OriginRefsShared {
                        remote: r.remote.clone(),
                        refspec: r.refspec.clone(),
                    })
            })
            .or_else(|| legacy_remote_refusal(&common_dir));
        let fetch = || {
            cx.git.output(dir, &args, net).map(drop).map_err(|e| {
                RemoteFailure::from_git_error(
                    e,
                    RefspecContext {
                        refspecs: &config.origin_fetch,
                        branch: entry.branch.as_deref(),
                    },
                )
            })
        };
        early.fetch = Some(refused.map_or_else(fetch, Err));
        early.fetch_time = start.elapsed();
    }
    // a fetch may have added shallow roots
    let shallow_roots = if early.fetch.is_some() {
        read_shallow_roots(&common_dir)
    } else {
        shallow_roots
    };
    let layout = Layout {
        shallow: !shallow_roots.is_empty(),
        sparse: config.sparse,
        partial_filter: config.partial_filter.clone(),
    };
    // before any step that reads objects: a partial clone may lack one
    early.layout = Some(layout.clone());

    // 2. status of the primary checkout
    let status = cx
        .git
        .output(dir, &STATUS_ARGS, local)
        .map_err(|e| e.to_string())?;
    let status = porcelain::parse_status(&status)?;

    // 3. branches
    let format = format!("--format={}", porcelain::REFS_FORMAT);
    let refs = cx
        .git
        .output(dir, &["for-each-ref", &format, "refs/heads"], local)
        .map_err(|e| e.to_string())?;
    let refs = porcelain::parse_refs(&refs)?;

    // 5. unique commits, where local work could be
    let mut branches = Vec::with_capacity(refs.len());
    for r in refs {
        let (unique_commits, on_fetched_tip) = if could_carry_local_work(&r) {
            count_unique(cx.git, dir, &r, &shallow_roots, local)?
        } else {
            (0, false)
        };
        branches.push(BranchFacts {
            branch: r,
            unique_commits,
            on_fetched_tip,
        });
    }

    // 6. the other worktrees
    let worktrees_dir = common_dir.join("worktrees");
    let mut worktrees = match std::fs::metadata(&worktrees_dir) {
        Ok(m) if m.is_dir() => {
            // only a branch whose upstream is gone can be cleaned up with the
            // worktree it's in
            let gone_branches: HashSet<&str> = branches
                .iter()
                .filter(|b| b.branch.track == Track::Gone)
                .map(|b| b.branch.name.as_str())
                .collect();
            probe_worktrees(cx.git, dir, &git_dir, &common_dir, &gone_branches, local)?
        }
        Ok(_) => Worktrees::default(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Worktrees::default(),
        Err(_) => Worktrees {
            unreadable: vec![worktrees_dir.to_string_lossy().into_owned()],
            ..Worktrees::default()
        },
    };

    // 7. files
    let primary_linked = canonical(&git_dir) != canonical(&common_dir);
    let in_progress = markers(&git_dir, &mut worktrees.unreadable);
    let fetched_at = newest_fetch(&git_dir, &common_dir, &worktrees.admins);
    let registry_worktrees = worktrees
        .probed
        .iter()
        .filter(|c| cx.registry_dirs.contains(Path::new(&c.path)))
        .map(|c| c.path.clone())
        .collect();
    let mut git_dirs = vec![(own_git_dir(&git_dir), path.clone())];
    git_dirs.append(&mut worktrees.git_dirs);
    let primary_locked = worktrees.primary_lock.is_some();
    let mut locks: Vec<(String, String)> = worktrees
        .primary_lock
        .map(|reason| (path.clone(), reason))
        .into_iter()
        .collect();
    locks.append(&mut worktrees.locks);

    Ok(Probed::Present(Box::new(RepoFacts {
        path,
        git_dir,
        common_dir,
        config,
        status,
        in_progress,
        primary_linked,
        primary_locked,
        locks,
        worktrees: worktrees.probed,
        registry_worktrees,
        unprobed: worktrees.unprobed,
        unreadable: worktrees.unreadable,
        relative_gitdir: worktrees.relative_gitdir,
        git_dirs,
        bare_main: worktrees.bare_main,
        branches,
        layout,
        fetched_at,
    })))
}

/// `status --fetch`'s fetch, before `--depth 1` (a shallow clone) and
/// `origin`: remote-tracking refs are all it may write, whatever the repo's
/// config says. The runner's `maintenance.auto=false` already keeps the
/// fetch from running `gc --auto` or maintenance.
const FETCH_ARGS: [&str; 9] = [
    // a configured bundle URI downloads bundles into `refs/bundles/*`
    "-c",
    "fetch.bundleURI=",
    "fetch",
    // a branch deleted upstream reads gone
    "--prune",
    "--quiet",
    // tag auto-follow, or `remote.<r>.tagOpt=--tags`, writes `refs/tags/*`;
    // a refspec naming `refs/tags/*` outright is refused before the fetch
    // (`refspec_writes_outside_origin`)
    "--no-tags",
    // `fetch.pruneTags` / `remote.<r>.pruneTags` delete every local tag
    // origin lacks, unpushed ones included
    "--no-prune-tags",
    // `fetch.recurseSubmodules` (default on-demand) and `submodule.recurse`
    // fetch populated submodules from their own URLs into `.git/modules/*`,
    // and fail the entry's fetch for a submodule's
    "--recurse-submodules=no",
    // `fetch.writeCommitGraph` writes `objects/info/commit-graph(s)`
    "--no-write-commit-graph",
];

/// Whether a fetch refspec writes a ref outside `refs/remotes/origin/`.
///
/// A local branch, a tag (`+refs/tags/*:refs/tags/*`, a mirror's
/// `+refs/*:refs/*`), another remote's tracking refs (`refs/remotes/*`,
/// `refs/remotes/upstream/*`, which `--prune` would then empty): anything a
/// flag can't switch off. A destination is taken as written, so a shorthand
/// one (`remotes/origin/*`) is refused too. A refspec with no destination
/// writes `FETCH_HEAD` alone, and a negative one writes nothing.
fn refspec_writes_outside_origin(refspec: &str) -> bool {
    refspec_destination(refspec).is_some_and(|dst| !dst.starts_with("refs/remotes/origin/"))
}

/// Whether another remote's fetch refspec may write under origin's
/// namespace, where origin's `--prune` would delete what it writes.
fn refspec_writes_into_origin(refspec: &str) -> bool {
    refspec_destination(refspec).is_some_and(destination_overlaps_origin)
}

/// Whether a destination may name a ref under `refs/remotes/origin/`, by
/// git's own reading of a fetch refspec's destination.
///
/// - **With a `*`** (git allows one; a second, or a leading `/`, is an
///   invalid refspec): git substitutes the matched text and uses the result
///   as written, no DWIM — a result not starting with `refs/` is ignored as a
///   funny ref. The match can be any text, a `/` included, so it overlaps
///   when the text before the `*` and `refs/remotes/origin/` share a prefix
///   either way round: `*`, `r*`, `ref*`, `refs*` (a remote's own
///   `refs/remotes/origin/z` lands as ours), `refs/remotes/*` (a branch named
///   `origin/y`), `refs/remotes/origin*`, `refs/remotes/o*/x` (a branch
///   `rigin`). The same text read under `refs/` counts too — git never does,
///   so that only refuses more (`remotes/origin/*`, which git ignores).
/// - **Without one**: git's DWIM — `refs/…` as written, `heads/`, `tags/`,
///   and `remotes/` under `refs/`, anything else under `refs/heads/` — then
///   by prefix: a remote named `origin/<x>` keeps its refs at
///   `refs/remotes/origin/<x>/*`.
///
/// Compared case-folded: on a case-insensitive file system
/// `refs/remotes/ORIGIN/x` is stored where origin's `x` is, so folding only
/// refuses more.
fn destination_overlaps_origin(dst: &str) -> bool {
    const ORIGIN: &str = "refs/remotes/origin/";
    let overlaps = |prefix: &str| prefix.starts_with(ORIGIN) || ORIGIN.starts_with(prefix);
    let dst = dst.to_ascii_lowercase();
    if let Some((prefix, _)) = dst.split_once('*') {
        return overlaps(prefix) || overlaps(&format!("refs/{prefix}"));
    }
    let full = if dst.starts_with("refs/") {
        dst
    } else if ["heads/", "tags/", "remotes/"]
        .iter()
        .any(|p| dst.starts_with(p))
    {
        format!("refs/{dst}")
    } else {
        format!("refs/heads/{dst}")
    };
    full.starts_with(ORIGIN)
}

/// The first legacy remote — a file under `<commondir>/remotes/`, which git
/// still honors — whose `Pull:` refspec may write under origin's namespace.
///
/// Fails closed: a `remotes/` dir, or a regular file in it, that can't be
/// read is refused too, since refs it once wrote may sit under origin.
/// Anything but a regular file (a dir, a FIFO) is skipped: git can't read a
/// remote from it. So is a file named `origin`: git reads it only when
/// config gives origin no URL, and then there's no fetch. `branches/` files
/// write `refs/heads/<name>`, never origin's.
fn legacy_remote_refusal(common_dir: &Path) -> Option<RemoteFailure> {
    let remotes = common_dir.join("remotes");
    let unreadable = |path: &Path| {
        Some(RemoteFailure::LegacyRemotesUnreadable {
            path: path.to_string_lossy().into_owned(),
        })
    };
    let entries = match std::fs::read_dir(&remotes) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return unreadable(&remotes),
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return unreadable(&remotes);
        };
        names.push(entry.file_name());
    }
    // deterministic: the first by name
    names.sort();
    for name in names {
        // git reads a legacy file only for a remote config gives no URL, and
        // origin is fetched only when config gives it one
        if name == "origin" {
            continue;
        }
        let path = remotes.join(&name);
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        // read as bytes (checked a regular file above, so no FIFO blocks):
        // a non-UTF-8 byte can't change which side of `refs/remotes/origin/`
        // a destination falls on
        let Ok(bytes) = std::fs::read(&path) else {
            return unreadable(&path);
        };
        let text = String::from_utf8_lossy(&bytes);
        let pull = text.lines().find_map(|line| {
            line.strip_prefix("Pull:")
                .map(str::trim)
                .filter(|refspec| refspec_writes_into_origin(refspec))
        });
        if let Some(refspec) = pull {
            return Some(RemoteFailure::OriginRefsShared {
                remote: name.to_string_lossy().into_owned(),
                refspec: refspec.to_owned(),
            });
        }
    }
    None
}

/// A positive refspec's destination, as written; `None` for a negative one
/// or one with no destination (`FETCH_HEAD` alone).
fn refspec_destination(refspec: &str) -> Option<&str> {
    if refspec.starts_with('^') {
        return None;
    }
    let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
    refspec
        .split_once(':')
        .map(|(_, dst)| dst)
        .filter(|dst| !dst.is_empty())
}

/// The flags step 2's status runs with, in every checkout.
const STATUS_ARGS: [&str; 8] = [
    "status",
    "--porcelain=v2",
    "--branch",
    "--show-stash",
    "--no-ahead-behind",
    "--no-renames",
    "--untracked-files=normal",
    "-z",
];

/// The repo's worktrees other than the primary.
#[derive(Debug, Default)]
struct Worktrees {
    probed: Vec<Checkout>,
    unprobed: Vec<UnprobedWorktree>,
    /// Git dirs whose in-progress markers couldn't be read.
    unreadable: Vec<String>,
    /// The primary's lock reason, when it's locked (only a linked
    /// worktree can be).
    primary_lock: Option<String>,
    /// Each other listed worktree's lock reason, with its path as `probed`
    /// or `unprobed` spells it.
    locks: Vec<(String, String)>,
    /// Every admin dir under `<commondir>/worktrees/`, each worktree's own
    /// git dir (where it keeps its `FETCH_HEAD`).
    admins: Vec<PathBuf>,
    /// The first admin dir whose `gitdir` is relative.
    relative_gitdir: Option<PathBuf>,
    /// Each worktree's own git dir that was found, canonicalized when it can
    /// be, with its path as `probed` or `unprobed` spells it.
    git_dirs: Vec<(PathBuf, String)>,
    /// The main worktree's path as git lists it, when the repo is bare.
    bare_main: Option<String>,
}

/// Probes every worktree of the repo but the primary: each one `git worktree
/// list` names (but a bare repo's main worktree, which has no files), and
/// each git dir under `<commondir>/worktrees/` the list leaves out. One that
/// can't be probed is recorded as unprobed rather than failing the entry,
/// with its operation read all the same when its git dir is found; a git dir
/// that can't be read is recorded as unreadable. `gone_branches` are the
/// branches whose upstream is gone, the only ones a worktree can be removed
/// with.
fn probe_worktrees(
    git: &Git,
    dir: &Path,
    git_dir: &Path,
    common_dir: &Path,
    gone_branches: &HashSet<&str>,
    opts: CallOptions<'_>,
) -> Result<Worktrees, String> {
    let out = git
        .output(dir, &["worktree", "list", "--porcelain", "-z"], opts)
        .map_err(|e| e.to_string())?;
    let records = porcelain::parse_worktrees(&out)?;
    let mut w = Worktrees::default();
    let admins = admin_dirs(common_dir).unwrap_or_else(|_| {
        w.unreadable
            .push(common_dir.join("worktrees").to_string_lossy().into_owned());
        Vec::new()
    });
    let mut used = vec![false; admins.len()];
    w.admins = admins.iter().map(|a| a.dir.clone()).collect();
    w.relative_gitdir = admins.iter().find(|a| a.relative).map(|a| a.dir.clone());
    // the primary is found by its git dir, never its path: with
    // `--separate-git-dir` git prints the main worktree's git dir as its path
    let primary_git_dir = canonical(git_dir);
    let primary_is_main = primary_git_dir.is_some() && primary_git_dir == canonical(common_dir);
    for (i, record) in records.into_iter().enumerate() {
        if record.head == WorktreeHead::Bare {
            w.bare_main = Some(record.path);
            continue;
        }
        // git lists the main worktree first, and its git dir is the common
        // dir; a linked one's is matched among the admin dirs
        let record_git_dir = if i == 0 {
            Some(common_dir.to_owned())
        } else {
            match_admin(&record, &admins, &mut used)
        };
        let is_primary = if primary_is_main {
            i == 0
        } else {
            i > 0 && record_git_dir.as_deref().and_then(canonical) == primary_git_dir
        };
        if is_primary {
            w.primary_lock = record.locked;
            continue;
        }
        let linked = i > 0;
        probe_record(
            git,
            record,
            record_git_dir.as_deref(),
            linked,
            gone_branches,
            &mut w,
        );
    }
    // git drops a worktree from its list when it can't read the worktree's
    // `gitdir` (missing, empty, unreadable) or its git dir: never silence
    for (admin, used) in admins.iter().zip(used) {
        if used || (primary_git_dir.is_some() && canonical(&admin.dir) == primary_git_dir) {
            continue;
        }
        let unlisted = unlisted_worktree(admin, &mut w.unreadable);
        w.git_dirs
            .push((own_git_dir(&admin.dir), unlisted.path.clone()));
        w.unprobed.push(unlisted);
    }
    Ok(w)
}

/// The admin dir that's a listed record's git dir: one not yet taken whose
/// `gitdir` names the record's path — preferring, when several do (a copied
/// admin dir), the one whose `HEAD` matches the record's, so each admin dir
/// serves one record. Records that tie share both path and HEAD, and the
/// probe's own `.git` check then decides which of them is the worktree.
fn match_admin(record: &WorktreeRecord, admins: &[AdminDir], used: &mut [bool]) -> Option<PathBuf> {
    let at = realpath_forgiving(Path::new(&record.path));
    let head = unprobed_head(&record.head);
    let mut best: Option<(usize, bool)> = None;
    for (j, admin) in admins.iter().enumerate() {
        if used[j] || !admin.worktree.as_ref().is_ok_and(|w| *w == at) {
            continue;
        }
        let head_matches = read_head(&admin.dir) == head;
        if best.is_none_or(|(_, b)| head_matches && !b) {
            best = Some((j, head_matches));
        }
    }
    let (j, _) = best?;
    used[j] = true;
    Some(admins[j].dir.clone())
}

/// Probes one listed worktree into `w`: a checkout, or unprobed with why.
fn probe_record(
    git: &Git,
    record: WorktreeRecord,
    git_dir: Option<&Path>,
    linked: bool,
    gone_branches: &HashSet<&str>,
    w: &mut Worktrees,
) {
    let path = PathBuf::from(&record.path);
    if let Some(d) = git_dir {
        w.git_dirs.push((own_git_dir(d), record.path.clone()));
    }
    let in_progress = git_dir.and_then(|d| markers(d, &mut w.unreadable));
    let locked = record.locked.is_some();
    if let Some(reason) = &record.locked {
        w.locks.push((record.path.clone(), reason.clone()));
    }
    let probed = gone(&path, record.prunable.is_some()).and_then(|()| {
        probe_worktree(git, &path, git_dir).map_err(|error| UnprobedWhy::Failed { error })
    });
    match probed {
        Ok(status) => {
            // only a worktree that could otherwise be removed with its branch
            // — one whose upstream is gone (`Merged` cleanup requires it not
            // checked out) — is worth the index read; a stale gone set only
            // fails closed (not removable); `classify` decides removability
            let candidate = linked
                && !locked
                && in_progress.is_none()
                && status.uncommitted.is_clean()
                && matches!(&status.head, Head::Branch { name } if gone_branches.contains(name.as_str()));
            // `probe_worktree` fails without a git dir, so `None` never
            // reaches here; it would count as submodules, failing closed
            let submodules =
                git_dir.map_or(Some(true), |d| submodule_refusal(git, &path, d, candidate));
            w.probed.push(Checkout {
                path: record.path,
                primary: false,
                linked,
                head: status.head,
                uncommitted: status.uncommitted,
                in_progress,
                locked,
                submodules,
                // filled once the live sessions are scoped (`status`)
                busy: Vec::new(),
            });
        }
        Err(why) => {
            // git lists a HEAD it couldn't read as detached, or with no head
            // at all: read it from the git dir, failing closed
            let head = match (&record.head, git_dir) {
                (WorktreeHead::Branch { name }, _) => UnprobedHead::Branch { name: name.clone() },
                (_, Some(d)) => read_head(d),
                (head, None) => unprobed_head(head),
            };
            // only a gone one's git dir is at stake, so only it's read — and
            // one that can't be matched can't be read at all
            let holds = git_dir
                .filter(|_| why == UnprobedWhy::Prunable)
                .map(|d| git_dir_holds(git, d, &head));
            w.unprobed.push(UnprobedWorktree {
                path: record.path,
                git_dir: git_dir.map(shown_git_dir),
                head,
                locked,
                in_progress,
                why,
                holds,
            });
        }
    }
}

/// An admin dir git's worktree list leaves out, as an unprobed worktree:
/// its path is the worktree its `gitdir` names when that's readable, else
/// the admin dir itself; its HEAD and markers come from the admin dir.
fn unlisted_worktree(admin: &AdminDir, unreadable: &mut Vec<String>) -> UnprobedWorktree {
    let (path, reason) = match &admin.worktree {
        Ok(worktree) => (
            worktree.clone(),
            format!("its git dir is {}", admin.dir.display()),
        ),
        Err(reason) => (admin.dir.clone(), reason.clone()),
    };
    UnprobedWorktree {
        path: path.to_string_lossy().into_owned(),
        git_dir: Some(shown_git_dir(&admin.dir)),
        head: read_head(&admin.dir),
        locked: admin.dir.join("locked").exists(),
        in_progress: markers(&admin.dir, unreadable),
        why: UnprobedWhy::Failed {
            error: format!("not listed by git: {reason}"),
        },
        holds: None,
    }
}

/// A record's head in `UnprobedHead`'s terms: to compare with an admin
/// dir's `HEAD` (`match_admin`), or as an unprobed worktree's head when its
/// git dir is unknown.
fn unprobed_head(head: &WorktreeHead) -> UnprobedHead {
    match head {
        WorktreeHead::Branch { name } => UnprobedHead::Branch { name: name.clone() },
        WorktreeHead::Detached { commit } => UnprobedHead::Detached {
            commit: commit.clone(),
        },
        WorktreeHead::Bare | WorktreeHead::Unknown => UnprobedHead::Unknown,
    }
}

/// Whether `git worktree remove` would refuse a worktree over submodules,
/// as git decides it: its git dir holds `modules/` (left even after
/// `deinit`), or a gitlink in its index (mode `160000`) is populated — its
/// `<path>/.git` exists — whether or not `.gitmodules` declares it. The
/// index is read only for a `candidate` (a worktree git could otherwise
/// remove); elsewhere, without `modules/`, it's `None`, unchecked. Anything
/// unreadable counts as yes.
fn submodule_refusal(git: &Git, path: &Path, git_dir: &Path, candidate: bool) -> Option<bool> {
    if git_dir.join("modules").try_exists().unwrap_or(true) {
        return Some(true);
    }
    if !candidate {
        return None;
    }
    let opts = CallOptions {
        ceiling: path.parent(),
        network: None,
    };
    let Ok(out) = git.output(path, &["ls-files", "--stage", "-z"], opts) else {
        return Some(true);
    };
    Some(porcelain::parse_gitlinks(&out).map_or(true, |gitlinks| any_populated(path, &gitlinks)))
}

/// Whether any gitlink under `path` has its `.git`; unreadable counts as
/// yes.
fn any_populated(path: &Path, gitlinks: &[String]) -> bool {
    gitlinks
        .iter()
        .any(|g| path.join(g).join(".git").try_exists().unwrap_or(true))
}

/// What a gone worktree's own git dir holds that its removal would drop,
/// read without writing: `modules/` and `refs/` by listing, the index
/// against HEAD by one `diff-index --cached` — not asked when its HEAD is
/// unknown, and none needed when it has no index (added `--no-checkout`).
fn git_dir_holds(git: &Git, git_dir: &Path, head: &UnprobedHead) -> GitDirHolds {
    let staged = match git_dir.join("index").try_exists() {
        Ok(false) => Some(false),
        Ok(true) if *head != UnprobedHead::Unknown => staged_changes(git, git_dir),
        Ok(true) | Err(_) => None,
    };
    GitDirHolds {
        submodules: holds_any(&git_dir.join("modules"), false),
        worktree_refs: holds_any(&git_dir.join("refs"), true),
        staged,
    }
}

/// Whether `dir` holds anything: any entry, or with `files`, anything but a
/// dir anywhere below it (links not followed). Missing holds nothing; one
/// that can't be read, or isn't a dir, holds something.
fn holds_any(dir: &Path, files: bool) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    for entry in entries {
        let Ok(entry) = entry else { return true };
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !files || !is_dir || holds_any(&entry.path(), true) {
            return true;
        }
    }
    false
}

/// Whether a worktree's index differs from its HEAD, from its git dir
/// alone: `diff-index --cached --quiet`, which compares the index with the
/// HEAD tree without refreshing or writing either; intent-to-add entries
/// (`git add -N`) hold no content, so they don't count. `None` when git
/// can't tell (a failure, or a path git can't be given).
fn staged_changes(git: &Git, git_dir: &Path) -> Option<bool> {
    let git_dir_arg = format!("--git-dir={}", git_dir.to_str()?);
    let args = [
        git_dir_arg.as_str(),
        "diff-index",
        "--cached",
        "--ita-invisible-in-index",
        "--quiet",
        "HEAD",
    ];
    let out = git.run(git_dir, &args, CallOptions::default()).ok()?;
    match out.status.code() {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    }
}

/// A path canonicalized, or `None` when it can't be.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// A checkout's own git dir as busy detection matches it: canonicalized
/// when it can be.
fn own_git_dir(git_dir: &Path) -> PathBuf {
    canonical(git_dir).unwrap_or_else(|| git_dir.to_owned())
}

/// A worktree's own git dir as the report carries it: canonicalized when it
/// can be, so it compares equal to the one a stray's `.git` names.
fn shown_git_dir(git_dir: &Path) -> String {
    own_git_dir(git_dir).to_string_lossy().into_owned()
}

/// `Err` with why a worktree can't be probed: its dir is gone (`Prunable`
/// when git says so, else `Missing`, as a locked worktree on unmounted media
/// is), or it's there but has no `.git` or can't be looked at (`Failed`).
/// Never `Prunable` while the dir exists: `git worktree prune` would delete
/// the git dir — index, HEAD, operation state — of files still on disk.
fn gone(path: &Path, prunable: bool) -> Result<(), UnprobedWhy> {
    let failed = |e: std::io::Error| UnprobedWhy::Failed {
        error: format!("checking {}: {e}", path.display()),
    };
    // before trusting git's prunable: git reads an unreadable `.git` as gone
    if path.join(".git").try_exists().map_err(failed)? {
        return Ok(());
    }
    if path.try_exists().map_err(failed)? {
        Err(UnprobedWhy::Failed {
            error: format!("{} has no .git", path.display()),
        })
    } else if prunable {
        Err(UnprobedWhy::Prunable)
    } else {
        Err(UnprobedWhy::Missing)
    }
}

/// Runs step 2's status in a worktree, after checking its `.git` points at
/// the git dir git's worktree list gave it — so a `.git` pointing into
/// another repo is a failure, not that repo's state.
fn probe_worktree(git: &Git, path: &Path, git_dir: Option<&Path>) -> Result<StatusFacts, String> {
    let git_dir = git_dir.ok_or("no git dir in this repo's worktrees names it")?;
    let dot_git = path.join(".git");
    let points_at = dot_git_target(&dot_git)?;
    let (Some(points_at), Some(git_dir)) = (canonical(&points_at), canonical(git_dir)) else {
        return Err(format!(
            "{} or its git dir can't be resolved",
            dot_git.display()
        ));
    };
    if points_at != git_dir {
        return Err(format!(
            "{} doesn't point at this repo's git dir for it",
            dot_git.display()
        ));
    }
    // discovery stops at the worktree itself, so with its `.git` gone git
    // fails rather than finding an enclosing repo — the primary, when the
    // worktree is nested in it; defense in depth: the check above already
    // rejects a missing `.git`, so no test reaches this
    let opts = CallOptions {
        ceiling: path.parent(),
        network: None,
    };
    let out = git
        .output(path, &STATUS_ARGS, opts)
        .map_err(|e| e.to_string())?;
    porcelain::parse_status(&out)
}

/// A linked worktree's own git dir, `<commondir>/worktrees/<id>`.
#[derive(Debug)]
pub(crate) struct AdminDir {
    pub(crate) dir: PathBuf,
    /// The worktree path its `gitdir` file names (relative to the git dir
    /// when git writes relative paths), resolved as far as it exists — how
    /// git's worktree list derives the path it prints, so the two compare
    /// equal even when the worktree is gone; `Err` with why it can't be read.
    pub(crate) worktree: Result<PathBuf, String>,
    /// Whether its `gitdir` names the worktree by a relative path, which git
    /// 2.48+ resolves against the git dir and older gits against the cwd.
    pub(crate) relative: bool,
    /// Whether its `gitdir` is there but can't be read (not missing, not
    /// empty), so the tool can't tell what worktree git names by it.
    pub(crate) unreadable: bool,
}

/// Every git dir under `<commondir>/worktrees/`, readable or not; entries
/// that aren't dirs are skipped, as git skips them (and prune deletes them).
///
/// # Errors
///
/// When `worktrees/` exists but can't be listed.
pub(crate) fn admin_dirs(common_dir: &Path) -> std::io::Result<Vec<AdminDir>> {
    let dirs = match std::fs::read_dir(common_dir.join("worktrees")) {
        Ok(dirs) => dirs,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut admins = Vec::new();
    for d in dirs {
        let dir = d?.path();
        // one that can't be stat'd stays, failing closed
        if std::fs::metadata(&dir).is_ok_and(|m| !m.is_dir()) {
            continue;
        }
        let gitdir = read_admin_gitdir(&dir);
        let relative = matches!(gitdir, AdminGitdir::Names { relative: true, .. });
        let unreadable = matches!(gitdir, AdminGitdir::Unreadable(_));
        let worktree = admin_worktree(&dir, gitdir);
        admins.push(AdminDir {
            dir,
            worktree,
            relative,
            unreadable,
        });
    }
    admins.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(admins)
}

/// What a linked worktree's git dir (`<commondir>/worktrees/<id>`) says in
/// its `gitdir` file, read as git reads it (`read_worktree_gitdir`), with the
/// read error's kind kept: a missing or empty file is lost (`git worktree
/// repair` rewrites it), any other failure may hide a worktree in use.
#[derive(Debug)]
pub(crate) enum AdminGitdir {
    /// The worktree path it names, as git takes it — raw bytes, trailing
    /// whitespace (git's own) trimmed, a trailing `/.git` stripped, then cut
    /// at the first NUL (`read_worktree_gitdir`) — joined to the git dir (git
    /// 2.48+ resolves a relative one there) and resolved as far as it
    /// exists: how git 2.48+'s worktree list derives the path it prints;
    /// `relative` when written relative, which older gits resolve against
    /// the cwd instead; `nul` when a NUL is left in it once trimmed, so a
    /// repair of the worktree reads it otherwise (`WorktreeGitdir::nul`).
    Names {
        worktree: PathBuf,
        relative: bool,
        nul: bool,
    },
    /// The file names no path: it's empty, or nothing's left once trimmed
    /// and cut.
    Empty,
    /// No `gitdir` file (the read's error, for messages).
    Missing(std::io::Error),
    Unreadable(std::io::Error),
}

/// Reads a linked worktree's git dir's `gitdir` file.
pub(crate) fn read_admin_gitdir(admin: &Path) -> AdminGitdir {
    match read_worktree_gitdir(admin) {
        Ok(written) if written.path.as_os_str().is_empty() => AdminGitdir::Empty,
        Ok(written) => AdminGitdir::Names {
            worktree: realpath_forgiving(&admin.join(&written.path)),
            relative: written.path.is_relative(),
            nul: written.nul,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AdminGitdir::Missing(e),
        Err(e) => AdminGitdir::Unreadable(e),
    }
}

/// The worktree path a linked worktree's git dir names, from its `gitdir`
/// as read; `Err` with why it can't be read.
fn admin_worktree(admin: &Path, gitdir: AdminGitdir) -> Result<PathBuf, String> {
    let file = admin.join("gitdir");
    match gitdir {
        AdminGitdir::Names { worktree, .. } => Ok(worktree),
        AdminGitdir::Empty => Err(format!("{} is empty", file.display())),
        // never `git worktree prune`: it prunes every gone worktree of the
        // repo, not just this one
        AdminGitdir::Missing(_) if !admin.join("HEAD").exists() => {
            let kept = kept_without_worktree(admin);
            Err(if kept.is_empty() {
                format!(
                    "{} holds no worktree (no gitdir, no HEAD); delete that dir by hand",
                    admin.display()
                )
            } else {
                format!(
                    "{} holds no worktree (no gitdir, no HEAD) but keeps {}; check it by hand",
                    admin.display(),
                    kept.join(" and ")
                )
            })
        }
        AdminGitdir::Missing(e) | AdminGitdir::Unreadable(e) => {
            Err(format!("reading {}: {e}", file.display()))
        }
    }
}

/// What a worktree git dir with neither `gitdir` nor `HEAD` still keeps
/// that deleting it would lose: a lock (`git worktree add` writes it first,
/// so the add may be under way, and `git worktree prune` skips it), an
/// index (maybe staged changes), an operation's state, submodules' repos,
/// per-worktree refs. Anything that can't be looked at counts as kept.
fn kept_without_worktree(admin: &Path) -> Vec<String> {
    let mut kept = Vec::new();
    if admin.join("locked").try_exists().unwrap_or(true) {
        kept.push("a lock (a git worktree add may be under way)".to_owned());
    }
    if admin.join("index").try_exists().unwrap_or(true) {
        kept.push("an index (maybe staged changes)".to_owned());
    }
    match read_in_progress(admin) {
        Ok(None) => {}
        Ok(Some(op)) => kept.push(format!("a {} in progress", op.label())),
        Err(_) => kept.push("operation markers it can't read".to_owned()),
    }
    if holds_any(&admin.join("modules"), false) {
        kept.push("submodules' repos (modules/)".to_owned());
    }
    if holds_any(&admin.join("refs"), true) {
        kept.push("per-worktree refs (refs/)".to_owned());
    }
    kept
}

/// `path` with its longest existing prefix canonicalized and the rest
/// appended as is.
fn realpath_forgiving(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = at.canonicalize() {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

/// The newest `FETCH_HEAD` mtime across the repo: each worktree fetches into
/// its own git dir — the primary's, the common dir (the main worktree's),
/// and every linked one's.
///
/// An empty `FETCH_HEAD` doesn't count. A fetch that fails (a missing ref, a
/// missing repo, no connection) still truncates it, freshening its mtime
/// over stale refs, while a successful fetch writes a line per ref it
/// fetched, changed or not. So a git dir whose last fetch failed reads as
/// never fetched — how old its remote-tracking refs are is unknown — and so
/// does one fetched only from an empty remote.
fn newest_fetch(git_dir: &Path, common_dir: &Path, admins: &[PathBuf]) -> Option<u64> {
    [git_dir.to_owned(), common_dir.to_owned()]
        .into_iter()
        .chain(admins.iter().cloned())
        .filter_map(|d| {
            let meta = std::fs::metadata(d.join("FETCH_HEAD")).ok()?;
            if meta.len() == 0 {
                return None;
            }
            meta.modified().ok()
        })
        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
        .max()
}

/// Why a dir isn't a repo: empty (a clone that never started), files with no
/// `.git` (a copy or an unpacked archive, not a clone), or git's message when
/// a `.git` is there but unusable (corrupt, dubious ownership, …).
fn not_a_repo_detail(dir: &Path, stderr: &str) -> String {
    if std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none()) {
        return "empty directory".into();
    }
    if dir.is_dir() && !dir.join(".git").exists() {
        return "no .git: a copy of the files, not a clone".into();
    }
    let line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(stderr)
        .trim();
    line.strip_prefix("fatal: ").unwrap_or(line).to_owned()
}

/// Counts a branch's commits on no remote. In a shallow clone every fetched
/// commit is a root (the clone recipe is depth 1), so roots are subtracted —
/// a tip fetched rather than made here isn't local work — and for what
/// remains, whether it sits on the fetched tip.
fn count_unique(
    git: &Git,
    dir: &Path,
    r: &RefFacts,
    shallow_roots: &HashSet<String>,
    opts: CallOptions<'_>,
) -> Result<(u32, bool), String> {
    let rev = format!("refs/heads/{}", r.name);
    if shallow_roots.is_empty() {
        let n = git
            .output_string(
                dir,
                &["rev-list", "--count", &rev, "--not", "--remotes"],
                opts,
            )
            .map_err(|e| e.to_string())?;
        let n = n
            .trim()
            .parse()
            .map_err(|_| format!("rev-list --count: `{}`", n.trim()))?;
        return Ok((n, false));
    }
    let out = git
        .output_string(dir, &["rev-list", &rev, "--not", "--remotes"], opts)
        .map_err(|e| e.to_string())?;
    let n = out.lines().filter(|c| !shallow_roots.contains(*c)).count();
    let n = u32::try_from(n).unwrap_or(u32::MAX);
    let on_fetched_tip = match (&r.upstream_ref, n) {
        (Some(upstream), 1..) => {
            let out = git
                .run(dir, &["merge-base", "--is-ancestor", upstream, &rev], opts)
                .map_err(|e| e.to_string())?;
            match out.status.code() {
                Some(0) => true,
                Some(1) => false,
                _ => {
                    return Err(format!(
                        "merge-base --is-ancestor failed: {}",
                        out.stderr.trim()
                    ));
                }
            }
        }
        _ => false,
    };
    Ok((n, on_fetched_tip))
}

/// The commits in `<commondir>/shallow`; empty for a full clone.
fn read_shallow_roots(common_dir: &Path) -> HashSet<String> {
    read_regular(&common_dir.join("shallow"))
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// An operation stopped mid-way, from its marker in the checkout's git dir.
/// Never `REBASE_HEAD`: git leaves it behind after a finished rebase.
///
/// # Errors
///
/// When a marker's presence can't be known — the git dir can't be read.
fn read_in_progress(git_dir: &Path) -> std::io::Result<Option<InProgressOp>> {
    for (marker, op) in [
        ("rebase-merge", InProgressOp::Rebase),
        // `git am` and the apply backend of `git rebase` share
        // `rebase-apply/`; am marks it `applying`, as git's own status reads
        ("rebase-apply/applying", InProgressOp::Am),
        ("rebase-apply", InProgressOp::Rebase),
        ("MERGE_HEAD", InProgressOp::Merge),
        ("CHERRY_PICK_HEAD", InProgressOp::CherryPick),
        ("REVERT_HEAD", InProgressOp::Revert),
        ("BISECT_LOG", InProgressOp::Bisect),
        ("sequencer", InProgressOp::Sequencer),
    ] {
        if git_dir.join(marker).try_exists()? {
            return Ok(Some(op));
        }
    }
    Ok(None)
}

/// `read_in_progress`, recording a git dir it can't read in `unreadable`.
fn markers(git_dir: &Path, unreadable: &mut Vec<String>) -> Option<InProgressOp> {
    read_in_progress(git_dir).unwrap_or_else(|_| {
        unreadable.push(git_dir.to_string_lossy().into_owned());
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    #[test]
    fn refspecs_writing_outside_remote_tracking_refs() {
        for refspec in [
            "+refs/tags/*:refs/tags/*",
            "+refs/*:refs/*",
            "+refs/heads/*:refs/heads/*",
            "refs/heads/main:refs/heads/main",
            "main:local",
            "+refs/heads/x:refs/bundles/x",
            // another remote's namespace, or all of them
            "+refs/heads/*:refs/remotes/*",
            "+refs/heads/*:refs/remotes/upstream/*",
            "+refs/heads/*:refs/remotes/origin2/*",
            // a shorthand destination: refused as written
            "+refs/heads/*:remotes/origin/*",
        ] {
            assert!(refspec_writes_outside_origin(refspec), "{refspec}");
        }
        for refspec in [
            "+refs/heads/*:refs/remotes/origin/*",
            "refs/heads/main:refs/remotes/origin/main",
            // a narrowed single-branch clone's
            "+refs/heads/feat:refs/remotes/origin/feat",
            "+refs/pull/*/head:refs/remotes/origin/pr/*",
            // `FETCH_HEAD` alone
            "feat",
            "refs/heads/feat",
            "refs/heads/feat:",
            // negative: writes nothing
            "^refs/heads/wip",
        ] {
            assert!(!refspec_writes_outside_origin(refspec), "{refspec}");
        }
    }

    #[test]
    fn other_remotes_writing_into_origins_namespace() {
        for refspec in [
            "+refs/heads/*:refs/remotes/origin/fork/*",
            "refs/heads/main:refs/remotes/origin/main",
            "+refs/heads/*:remotes/origin/fork/*",
            // a `*` that can expand across the `/`
            "+refs/heads*:refs/remotes/origin*",
            "+refs/heads/*:refs/remotes/*",
            "+refs/heads/*:refs/remotes/orig*",
            "+refs/*:refs/*",
            "+refs/remotes/*:refs/remotes/*",
            "+refs/heads/*:remotes/*",
            // full-name globs: substituted as written, no DWIM
            "+ref*:ref*",
            "+refs*:refs*",
            "+*:*",
            "+r*:r*",
            "+refs/heads/*:*",
            // a `*` mid-path: a branch `rigin` lands at `origin/x`
            "+refs/heads/*:refs/remotes/o*/x",
            // git ignores these (funny refs); refused anyway
            "+refs/heads/*:remotes/origin/*",
            // a non-`*` shorthand git DWIMs under `refs/`
            "refs/heads/fx:remotes/origin/fx",
            // case-folded, for case-insensitive file systems
            "+refs/heads/*:refs/remotes/ORIGIN/*",
            "refs/heads/fx:Refs/Remotes/Origin/fx",
        ] {
            assert!(refspec_writes_into_origin(refspec), "{refspec}");
        }
        // what a legacy `Pull:` line carries is the same refspec syntax
        for refspec in ["+ref*:ref*", "+refs*:refs*"] {
            assert!(refspec_writes_into_origin(refspec), "{refspec}");
        }
        for refspec in [
            // what an `upstream` remote carries in the real workspace
            "+refs/heads/*:refs/remotes/upstream/*",
            "+refs/heads/*:refs/remotes/origin2/*",
            "+refs/heads/*:refs/remotes/originals/*",
            "+refs/heads/*:refs/remotes/up*",
            "refs/heads/main:refs/remotes/origin",
            "+refs/tags/*:refs/tags/*",
            "+refs/heads/*:tags*",
            "+refs/heads/*:refs/remotes/x*",
            // git DWIMs a bare name under `refs/heads/`
            "refs/heads/fx:origin/fx",
            "refs/heads/fx:heads/fx2",
            "feat",
            "^refs/remotes/origin/x",
        ] {
            assert!(!refspec_writes_into_origin(refspec), "{refspec}");
        }
    }

    #[test]
    fn origins_own_globs_stay_in_its_namespace() {
        assert!(!refspec_writes_outside_origin(
            "+refs/heads/*:refs/remotes/origin/*"
        ));
        for refspec in [
            "+refs/heads/*:refs/remotes/origin*",
            "+refs/heads/*:refs/remotes/*",
            "+refs/*:refs/*",
        ] {
            assert!(refspec_writes_outside_origin(refspec), "{refspec}");
        }
    }

    #[test]
    fn legacy_remotes_writing_into_origin_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path();
        assert_eq!(legacy_remote_refusal(common), None);
        let remotes = common.join("remotes");
        std::fs::create_dir(&remotes).unwrap();
        std::fs::write(
            remotes.join("fine"),
            "URL: file:///x\nPull: +refs/heads/*:refs/remotes/fine/*\n",
        )
        .unwrap();
        // a dir there is no remote
        std::fs::create_dir(remotes.join("adir")).unwrap();
        // nor is a legacy `origin`: git reads it only when config gives origin
        // no URL, and then there's no fetch
        std::fs::write(
            remotes.join("origin"),
            "URL: file:///x\nPull: +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        // a non-UTF-8 byte doesn't make a harmless file unreadable
        std::fs::write(
            remotes.join("bytes"),
            b"URL: file:///\xff\nPull: +refs/heads/*:refs/remotes/b\xffs/*\n",
        )
        .unwrap();
        assert_eq!(legacy_remote_refusal(common), None);
        std::fs::write(
            remotes.join("legacy"),
            "URL: file:///x\nPull:  +refs/heads/fx:refs/remotes/origin/fork-fx\n",
        )
        .unwrap();
        assert_eq!(
            legacy_remote_refusal(common),
            Some(RemoteFailure::OriginRefsShared {
                remote: "legacy".into(),
                refspec: "+refs/heads/fx:refs/remotes/origin/fork-fx".into(),
            })
        );
        // fails closed on what can't be read: a file, then the dir itself
        std::fs::remove_file(remotes.join("legacy")).unwrap();
        let sealed = remotes.join("sealed");
        std::fs::write(&sealed, "Pull: +refs/heads/*:refs/remotes/fine/*\n").unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&sealed).is_ok() {
            eprintln!("skipped: permissions don't bind this user (root)");
            return;
        }
        assert_eq!(
            legacy_remote_refusal(common),
            Some(RemoteFailure::LegacyRemotesUnreadable {
                path: sealed.to_string_lossy().into_owned()
            })
        );
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&remotes, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refusal = legacy_remote_refusal(common);
        std::fs::set_permissions(&remotes, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            refusal,
            Some(RemoteFailure::LegacyRemotesUnreadable {
                path: remotes.to_string_lossy().into_owned()
            })
        );
    }

    fn r(upstream: Option<&str>, track: Track) -> RefFacts {
        RefFacts {
            name: "b".into(),
            upstream_ref: upstream.map(str::to_owned),
            track,
            worktree: None,
            committer_time: 0,
        }
    }

    #[test]
    fn local_work_is_possible_off_an_even_or_behind_upstream() {
        let up = Some("refs/remotes/origin/b");
        assert!(!could_carry_local_work(&r(up, Track::Even)));
        assert!(!could_carry_local_work(&r(up, Track::Behind(3))));
        assert!(could_carry_local_work(&r(up, Track::Ahead(1))));
        assert!(could_carry_local_work(&r(
            up,
            Track::Diverged {
                ahead: 1,
                behind: 1
            }
        )));
        assert!(could_carry_local_work(&r(up, Track::Gone)));
        assert!(could_carry_local_work(&r(None, Track::Even)));
    }

    #[test]
    fn in_progress_ignores_a_stale_rebase_head() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("REBASE_HEAD"), "abc\n").unwrap();
        assert_eq!(read_in_progress(tmp.path()).unwrap(), None);
        std::fs::create_dir(tmp.path().join("rebase-merge")).unwrap();
        assert_eq!(
            read_in_progress(tmp.path()).unwrap(),
            Some(InProgressOp::Rebase)
        );
    }

    #[test]
    fn in_progress_tells_am_from_an_apply_rebase() {
        let tmp = tempfile::tempdir().unwrap();
        let apply = tmp.path().join("rebase-apply");
        std::fs::create_dir(&apply).unwrap();
        assert_eq!(
            read_in_progress(tmp.path()).unwrap(),
            Some(InProgressOp::Rebase)
        );
        std::fs::write(apply.join("applying"), "").unwrap();
        assert_eq!(
            read_in_progress(tmp.path()).unwrap(),
            Some(InProgressOp::Am)
        );
    }

    #[test]
    fn an_unlisted_worktree_takes_its_path_from_a_readable_gitdir() {
        let tmp = tempfile::tempdir().unwrap();
        let admin = tmp.path().join("wt");
        std::fs::create_dir(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/feat\n").unwrap();
        std::fs::create_dir(admin.join("rebase-merge")).unwrap();
        let mut unreadable = Vec::new();
        // named by a readable `gitdir`: its worktree's path
        let named = AdminDir {
            dir: admin.clone(),
            worktree: Ok(PathBuf::from("/ws/app-feat")),
            relative: false,
            unreadable: false,
        };
        let u = unlisted_worktree(&named, &mut unreadable);
        assert_eq!(u.path, "/ws/app-feat");
        assert_eq!(
            u.why,
            UnprobedWhy::Failed {
                error: format!("not listed by git: its git dir is {}", admin.display())
            }
        );
        assert_eq!(
            u.head,
            UnprobedHead::Branch {
                name: "feat".into()
            }
        );
        assert_eq!(u.in_progress, Some(InProgressOp::Rebase));
        // not: the git dir itself
        let unnamed = AdminDir {
            dir: admin.clone(),
            worktree: Err("gone".into()),
            relative: false,
            unreadable: false,
        };
        let u = unlisted_worktree(&unnamed, &mut unreadable);
        assert_eq!(u.path, admin.to_str().unwrap());
        assert_eq!(
            u.why,
            UnprobedWhy::Failed {
                error: "not listed by git: gone".into()
            }
        );
        assert!(unreadable.is_empty());
    }

    #[test]
    fn a_failed_record_takes_its_head_from_its_git_dir() {
        // git read the HEAD for its list, and it changed since: the git dir
        // is what's there now
        let tmp = tempfile::tempdir().unwrap();
        let admin = tmp.path().join("admin");
        std::fs::create_dir(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "garbage\n").unwrap();
        let record = WorktreeRecord {
            // gone: probed without a git call
            path: tmp.path().join("gone").to_string_lossy().into_owned(),
            head: WorktreeHead::Detached {
                commit: "3890426260f93bbbdf34262c865872c38302836e".into(),
            },
            locked: Some(String::new()),
            prunable: None,
        };
        let git = Git::new();
        let mut w = Worktrees::default();
        probe_record(&git, record, Some(&admin), true, &HashSet::new(), &mut w);
        assert_eq!(git.spawns(), 0);
        assert_eq!(w.unprobed.len(), 1);
        assert_eq!(w.unprobed[0].head, UnprobedHead::Unknown);
        assert_eq!(w.unprobed[0].why, UnprobedWhy::Missing);
    }

    #[test]
    fn submodules_that_cannot_be_looked_at_count() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let sealed = tmp.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = sealed.join("x").try_exists().is_err();
        let result = (
            any_populated(tmp.path(), &["sealed/nested".to_owned()]),
            any_populated(tmp.path(), &["absent".to_owned()]),
            // a git dir whose `modules/` can't be checked, and no index read
            submodule_refusal(&Git::new(), tmp.path(), &sealed.join("admin"), false),
        );
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !unreadable {
            eprintln!("skipped: permissions don't bind this user (root)");
            return;
        }
        assert_eq!(result, (true, false, Some(true)));
    }

    /// A runner that sees no global or system config.
    fn hermetic_git() -> Git {
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        env.extend(std::env::var_os("PATH").map(|p| ("PATH".into(), p)));
        Git::with_clean_env(env)
    }

    /// `git init` plus a gitlink named `name` in the index, never populated.
    fn repo_with_gitlink(dir: &Path, name: &std::ffi::OsStr) {
        let git = |args: &[&std::ffi::OsStr]| {
            let status = std::process::Command::new("git")
                .env_clear()
                .envs(std::env::var_os("PATH").map(|p| ("PATH", p)))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        std::fs::create_dir(dir).unwrap();
        git(&["init".as_ref(), "-q".as_ref()]);
        let mut info = std::ffi::OsString::from("160000,3890426260f93bbbdf34262c865872c38302836e,");
        info.push(name);
        git(&[
            "update-index".as_ref(),
            "--add".as_ref(),
            "--cacheinfo".as_ref(),
            info.as_os_str(),
        ]);
    }

    #[test]
    fn a_gitlink_list_that_cannot_be_read_refuses() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let git = hermetic_git();
        // not a repo: `ls-files` fails
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(
            submodule_refusal(&git, &plain, &plain.join(".git"), true),
            Some(true)
        );
        // a gitlink whose name isn't UTF-8: the list can't be parsed
        let odd = tmp.path().join("odd");
        repo_with_gitlink(&odd, std::ffi::OsStr::from_bytes(b"sub\xff"));
        assert_eq!(
            submodule_refusal(&git, &odd, &odd.join(".git"), true),
            Some(true)
        );
        // control: a UTF-8 gitlink, never populated, doesn't refuse
        let fine = tmp.path().join("fine");
        repo_with_gitlink(&fine, "sub".as_ref());
        assert_eq!(
            submodule_refusal(&git, &fine, &fine.join(".git"), true),
            Some(false)
        );
    }

    #[test]
    fn not_a_repo_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Registry::parse(
            r#"
owners = ["me"]
[repos.empty]
url = "https://github.com/me/empty"
visibility = "public"
purpose = "a clone that never started"
[repos.plain]
url = "https://github.com/me/plain"
visibility = "public"
purpose = "a dir that isn't a checkout"
[repos.stub]
url = "https://github.com/me/stub"
visibility = "public"
purpose = "a .git git can't use"
"#,
        )
        .unwrap()
        .validate()
        .unwrap();
        std::fs::create_dir(tmp.path().join("empty")).unwrap();
        std::fs::create_dir(tmp.path().join("plain")).unwrap();
        std::fs::write(tmp.path().join("plain/file"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join("stub/.git")).unwrap();
        let git = Git::new();
        let registry_dirs = RegistryDirs::default();
        let cx = ProbeContext {
            git: &git,
            root: tmp.path(),
            registry_dirs: &registry_dirs,
            fetch: false,
        };
        let details: Vec<String> = registry
            .entries()
            .iter()
            .map(|e| match probe(e, cx).probed {
                Probed::NotARepo { detail } => detail,
                p => panic!("{}: {p:?}", e.key),
            })
            .collect();
        assert_eq!(details[0], "empty directory");
        assert_eq!(details[1], "no .git: a copy of the files, not a clone");
        assert!(
            details[2].contains("not a git repository"),
            "{}",
            details[2]
        );
    }
}
