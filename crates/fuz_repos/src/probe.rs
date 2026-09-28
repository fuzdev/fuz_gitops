//! The per-entry probe: the git calls and file reads behind `classify`.
//!
//! Nothing here writes a working tree, an index, or a
//! local branch; only the optional fetch touches the network, and it writes
//! remote-tracking refs alone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::porcelain::{
    self, ConfigFacts, RefFacts, StatusFacts, Track, WorktreeHead, WorktreeRecord,
};
use crate::registry::{CheckoutMode, Entry};
use crate::state::{
    Checkout, Head, InProgressOp, Layout, UnprobedHead, UnprobedWhy, UnprobedWorktree,
};

/// What the probe needs from its caller.
#[derive(Debug, Clone, Copy)]
pub struct ProbeContext<'a> {
    pub git: &'a Git,
    pub root: &'a Path,
    /// Fetch owned, non-pinned entries from `origin` before probing.
    pub fetch: bool,
}

/// One entry's probe, with its timings.
#[derive(Debug)]
pub struct ProbeRun {
    pub probed: Probed,
    /// `None` when no fetch was attempted; `Some(Err)` carries git's message.
    pub fetch: Option<Result<(), String>>,
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
    /// The dir is a repo, but a later call failed.
    Failed {
        error: String,
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
    /// The repo's other worktrees probed — linked ones, and the main one
    /// when the primary is linked — in `git worktree list` order.
    pub worktrees: Vec<Checkout>,
    /// The worktrees that couldn't be probed: listed but gone or failing,
    /// or unlisted.
    pub unprobed: Vec<UnprobedWorktree>,
    /// Git dirs whose in-progress markers couldn't be read — a worktree's,
    /// or `<commondir>/worktrees/` itself — so an operation there is
    /// unknowable.
    pub unreadable: Vec<String>,
    pub branches: Vec<BranchFacts>,
    pub layout: Layout,
    /// The newest `FETCH_HEAD` mtime across the repo's worktrees (each keeps
    /// its own), in unix seconds; `None` when none was ever fetched.
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

/// Whether `--fetch` fetches this entry: owned and not pinned.
pub fn fetches(entry: &Entry) -> bool {
    entry.writable && entry.checkout_mode != CheckoutMode::Pinned
}

/// Probes one entry.
pub fn probe(entry: &Entry, cx: ProbeContext<'_>) -> ProbeRun {
    let start = Instant::now();
    let mut fetch = FetchRun::default();
    let probed = probe_present(entry, &cx.root.join(&entry.dir), cx, &mut fetch)
        .unwrap_or_else(|error| Probed::Failed { error });
    ProbeRun {
        probed,
        fetch: fetch.result,
        fetch_time: fetch.time,
        probe_time: start.elapsed().saturating_sub(fetch.time),
    }
}

/// The fetch step's outcome, recorded even when a later step fails.
#[derive(Debug, Default)]
struct FetchRun {
    result: Option<Result<(), String>>,
    time: Duration,
}

fn probe_present(
    entry: &Entry,
    dir: &Path,
    cx: ProbeContext<'_>,
    fetch: &mut FetchRun,
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
    let config = match cx.git.run(
        dir,
        &["config", "-z", "--get-regexp", porcelain::CONFIG_PATTERN],
        local,
    ) {
        // exit 1 is "no matching keys"
        Ok(out) if out.status.success() || out.status.code() == Some(1) => {
            ConfigFacts::parse(&out.stdout)?
        }
        Ok(out) => return Err(format!("config failed: {}", out.stderr.trim())),
        Err(e) => return Err(e.to_string()),
    };
    let shallow_roots = read_shallow_roots(&common_dir);

    if cx.fetch && fetches(entry) {
        let start = Instant::now();
        let mut args = vec!["fetch", "--prune", "--quiet"];
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
        fetch.result = Some(
            cx.git
                .output(dir, &args, net)
                .map(drop)
                .map_err(|e| match e {
                    GitError::Failed { stderr, .. } => stderr,
                    e => e.to_string(),
                }),
        );
        fetch.time = start.elapsed();
    }
    // a fetch may have added shallow roots
    let shallow_roots = if fetch.result.is_some() {
        read_shallow_roots(&common_dir)
    } else {
        shallow_roots
    };

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
    let layout = Layout {
        shallow: !shallow_roots.is_empty(),
        sparse: config.sparse,
        partial_filter: config.partial_filter.clone(),
    };

    Ok(Probed::Present(Box::new(RepoFacts {
        path,
        git_dir,
        common_dir,
        config,
        status,
        in_progress,
        primary_linked,
        primary_locked: worktrees.primary_locked,
        worktrees: worktrees.probed,
        unprobed: worktrees.unprobed,
        unreadable: worktrees.unreadable,
        branches,
        layout,
        fetched_at,
    })))
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
    /// Whether the primary is locked (only a linked worktree can be).
    primary_locked: bool,
    /// Every admin dir under `<commondir>/worktrees/`, each worktree's own
    /// git dir (where it keeps its `FETCH_HEAD`).
    admins: Vec<PathBuf>,
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
    // the primary is found by its git dir, never its path: with
    // `--separate-git-dir` git prints the main worktree's git dir as its path
    let primary_git_dir = canonical(git_dir);
    let primary_is_main = primary_git_dir.is_some() && primary_git_dir == canonical(common_dir);
    for (i, record) in records.into_iter().enumerate() {
        if record.head == WorktreeHead::Bare {
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
            w.primary_locked = record.locked.is_some();
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
    let in_progress = git_dir.and_then(|d| markers(d, &mut w.unreadable));
    let locked = record.locked.is_some();
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
            w.unprobed.push(UnprobedWorktree {
                path: record.path,
                head,
                locked,
                in_progress,
                why,
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
        head: read_head(&admin.dir),
        locked: admin.dir.join("locked").exists(),
        in_progress: markers(&admin.dir, unreadable),
        why: UnprobedWhy::Failed {
            error: format!("not listed by git: {reason}"),
        },
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

/// A path canonicalized, or `None` when it can't be.
fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// A worktree's HEAD, from its git dir: `Unknown` when it can't be read or
/// is neither a branch ref nor a full object id.
fn read_head(admin: &Path) -> UnprobedHead {
    let Ok(head) = std::fs::read_to_string(admin.join("HEAD")) else {
        return UnprobedHead::Unknown;
    };
    let head = head.trim();
    if let Some(name) = head.strip_prefix("ref: refs/heads/") {
        return UnprobedHead::Branch { name: name.into() };
    }
    if porcelain::is_object_id(head) {
        return UnprobedHead::Detached {
            commit: head.to_owned(),
        };
    }
    UnprobedHead::Unknown
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

/// The git dir a worktree's `.git` names: the dir itself, or a `.git` file's
/// `gitdir:` line (relative to the worktree when git writes relative paths).
fn dot_git_target(dot_git: &Path) -> Result<PathBuf, String> {
    if dot_git.is_dir() {
        return Ok(dot_git.to_owned());
    }
    let content = std::fs::read_to_string(dot_git)
        .map_err(|e| format!("reading {}: {e}", dot_git.display()))?;
    let target = content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))
        .ok_or_else(|| format!("{} has no gitdir line", dot_git.display()))?;
    let base = dot_git.parent().unwrap_or(dot_git);
    Ok(base.join(target.trim_end()))
}

/// A linked worktree's own git dir, `<commondir>/worktrees/<id>`.
#[derive(Debug)]
struct AdminDir {
    dir: PathBuf,
    /// The worktree path its `gitdir` file names (relative to the git dir
    /// when git writes relative paths), resolved as far as it exists — how
    /// git's worktree list derives the path it prints, so the two compare
    /// equal even when the worktree is gone; `Err` with why it can't be read.
    worktree: Result<PathBuf, String>,
}

/// Every git dir under `<commondir>/worktrees/`, readable or not; entries
/// that aren't dirs are skipped, as git skips them (and prune deletes them).
///
/// # Errors
///
/// When `worktrees/` exists but can't be listed.
fn admin_dirs(common_dir: &Path) -> std::io::Result<Vec<AdminDir>> {
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
        let file = dir.join("gitdir");
        let worktree = match std::fs::read_to_string(&file) {
            Ok(target) if target.trim().is_empty() => Err(format!("{} is empty", file.display())),
            Ok(target) => {
                let dot_git = dir.join(target.trim_end());
                Ok(realpath_forgiving(dot_git.parent().unwrap_or(&dot_git)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !dir.join("HEAD").exists() => {
                Err(format!(
                    "{} holds no worktree (no gitdir, no HEAD); git worktree prune removes it",
                    dir.display()
                ))
            }
            Err(e) => Err(format!("reading {}: {e}", file.display())),
        };
        admins.push(AdminDir { dir, worktree });
    }
    admins.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(admins)
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
fn newest_fetch(git_dir: &Path, common_dir: &Path, admins: &[PathBuf]) -> Option<u64> {
    [git_dir.to_owned(), common_dir.to_owned()]
        .into_iter()
        .chain(admins.iter().cloned())
        .filter_map(|d| {
            std::fs::metadata(d.join("FETCH_HEAD"))
                .ok()?
                .modified()
                .ok()
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
    std::fs::read_to_string(common_dir.join("shallow"))
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
    fn read_head_trusts_only_branches_and_object_ids() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_head(tmp.path()), UnprobedHead::Unknown);
        let oid = "3890426260f93bbbdf34262c865872c38302836e";
        for (content, want) in [
            (
                "ref: refs/heads/main\n",
                UnprobedHead::Branch {
                    name: "main".into(),
                },
            ),
            (
                &*format!("{oid}\n"),
                UnprobedHead::Detached { commit: oid.into() },
            ),
            ("garbage\n", UnprobedHead::Unknown),
            ("abc123\n", UnprobedHead::Unknown),
            (
                "0000000000000000000000000000000000000000\n",
                UnprobedHead::Unknown,
            ),
            ("ref: refs/remotes/origin/main\n", UnprobedHead::Unknown),
        ] {
            std::fs::write(tmp.path().join("HEAD"), content).unwrap();
            assert_eq!(read_head(tmp.path()), want, "{content}");
        }
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
        .unwrap();
        std::fs::create_dir(tmp.path().join("empty")).unwrap();
        std::fs::create_dir(tmp.path().join("plain")).unwrap();
        std::fs::write(tmp.path().join("plain/file"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join("stub/.git")).unwrap();
        let git = Git::new();
        let cx = ProbeContext {
            git: &git,
            root: tmp.path(),
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
