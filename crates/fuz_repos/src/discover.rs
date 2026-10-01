//! Finding the registry, the workspace root it names, and the entries a
//! command's targets select.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::classify::origin_matches;
use crate::error::{Error, Result};
use crate::git::{CallOptions, Git, GitError};
use crate::gitdir::dot_git_target;
use crate::paths::same_canonical;
use crate::registry::{Entry, RegistryDirs, ValidRegistry};

/// The registry's file name, found by walking up from the cwd.
pub const REGISTRY_FILE: &str = "repos.toml";

/// Where the caller says the registry and the workspace root are
/// (`--registry`, `--root`), each relative to the cwd; `None` finds them
/// (`find_registry`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Locate<'a> {
    pub registry: Option<&'a Path>,
    pub root: Option<&'a Path>,
}

/// A workspace loaded: its registry found, validated, and its root vouched
/// for (`Workspace::load`) — what every command starts from.
#[derive(Debug)]
pub struct Workspace {
    pub location: RegistryLocation,
    pub registry: ValidRegistry,
    /// Every entry, as `ValidRegistry::entries` orders them.
    pub entries: Vec<Entry>,
}

impl Workspace {
    /// Checks that git is there and new enough, finds the registry
    /// (`find_registry`: walking up from `start`, unless `locate` names it,
    /// relative to `cwd`), loads and validates it, and refuses a root
    /// found walking up that is an entry's checkout
    /// (`check_discovered_root`).
    ///
    /// # Errors
    ///
    /// `GitNotFound` or `GitTooOld` first, since discovery runs git too;
    /// then what `find_registry` and `ValidRegistry::load` return, and
    /// `RootInEntry`.
    pub fn load(git: &Git, cwd: &Path, start: &Path, locate: Locate<'_>) -> Result<Self> {
        git.check_version(cwd)?;
        let registry = locate.registry.map(|r| cwd.join(r));
        let root = locate.root.map(|r| cwd.join(r));
        let location = find_registry(start, registry.as_deref(), root.as_deref(), git)?;
        // validated before targets resolve and anything is probed
        let registry = ValidRegistry::load(&location.path)?;
        let entries = registry.entries();
        check_discovered_root(&location, &entries, git)?;
        Ok(Self {
            location,
            registry,
            entries,
        })
    }

    /// The workspace root entry dirs resolve against.
    pub fn root(&self) -> &Path {
        &self.location.root
    }

    /// Every entry's dir, canonicalized (`RegistryDirs`).
    pub fn registry_dirs(&self) -> RegistryDirs {
        RegistryDirs::new(&self.location.root, &self.entries)
    }
}

/// Where the registry was found.
///
/// `root` is the directory holding `path` as found — never the target of a
/// symlinked registry. A registry found inside a checkout is found where a
/// link above that checkout names it, when one does (`walk_up`).
#[derive(Debug, Clone)]
pub struct RegistryLocation {
    pub path: PathBuf,
    pub root: PathBuf,
    /// The root was found walking up — neither `--registry` nor `--root`
    /// named it — so `check_discovered_root` applies.
    pub discovered: bool,
}

/// Locates the registry and the workspace root.
///
/// The registry is `explicit` if given (relative to `cwd`), its dir taken
/// as the root as it stands — explicit is explicit, links or not. Else it's
/// the first `repos.toml` in `cwd` or an ancestor, read physically (`cwd`
/// canonicalized), and placed by `walk_up` — no env var, no `$HOME`
/// fallback. When that walk finds nothing and `cwd` is inside a linked
/// worktree, it runs once more from the repo's main checkout, so a linked
/// worktree outside the workspace finds its repo's registry. The root is
/// `root` if given (relative to `cwd`), for a registry kept outside the
/// workspace, else the registry's dir.
///
/// # Errors
///
/// `RegistryNotFound` when neither walk finds a registry (or `cwd` doesn't
/// exist); `RootNotFound` when `root` isn't a directory — a mistyped root
/// would otherwise read every entry as missing.
pub fn find_registry(
    cwd: &Path,
    explicit: Option<&Path>,
    root: Option<&Path>,
    git: &Git,
) -> Result<RegistryLocation> {
    let mut found = if let Some(explicit) = explicit {
        let path = cwd.join(explicit);
        let root = path.parent().map_or_else(|| cwd.to_owned(), Path::to_owned);
        RegistryLocation {
            path,
            root,
            discovered: false,
        }
    } else {
        cwd.canonicalize()
            .ok()
            .and_then(|start| {
                walk_up(&start, git)
                    .or_else(|| walk_up(&main_checkout(&start, false, git)?.main, git))
            })
            .ok_or_else(|| Error::RegistryNotFound {
                start: cwd.to_owned(),
            })?
    };
    if let Some(root) = root {
        let root = cwd.join(root);
        if !root.is_dir() {
            return Err(Error::RootNotFound { root });
        }
        found.root = root;
        found.discovered = false;
    }
    Ok(found)
}

/// The first `repos.toml` in `start` or an ancestor, placed at the workspace
/// root it belongs to.
///
/// A registry is often kept in one of the workspace's own repos and linked
/// at the workspace root. Found outside any checkout, it stays where it's
/// found. Found inside a checkout — the nearest dir at or above it with a
/// `.git`, read without git — the root is the nearest ancestor strictly
/// above that checkout whose `repos.toml` is the same file: the same device
/// and inode, followed through symlinks, so a hard link counts and no path
/// spelling matters. Nearest, so the innermost workspace wins: a stray link
/// further out never captures it. A link inside the checkout doesn't root
/// it (a repo isn't a workspace), a different file above it (an unrelated
/// registry) neither roots it nor stops the search, and one that can't be
/// read (dangling, a loop, no permission) is passed over — doubt never
/// moves the root. With no link above, it stays where it's found, and
/// `check_discovered_root` refuses it if that checkout is an entry's.
///
/// A registry committed in its repo has a copy in each linked worktree,
/// found first from inside one and linked nowhere. So when no link above a
/// linked worktree names its copy, the search runs from the same place in
/// the repo's main checkout (`linked_from_main_checkout`): a link above the
/// main checkout to its copy roots the workspace, which then reads that
/// copy, not the worktree's.
fn walk_up(start: &Path, git: &Git) -> Option<RegistryLocation> {
    let dir = start
        .ancestors()
        .find(|dir| dir.join(REGISTRY_FILE).is_file())?;
    let root = checkout_holding(dir)
        .and_then(|(top, linked)| {
            nearest_link_above(top, &dir.join(REGISTRY_FILE)).or_else(|| {
                linked
                    .then(|| linked_from_main_checkout(dir, git))
                    .flatten()
            })
        })
        .unwrap_or_else(|| dir.to_owned());
    Some(RegistryLocation {
        path: root.join(REGISTRY_FILE),
        root,
        discovered: true,
    })
}

/// The checkout holding `dir`, read without git: the nearest dir at or
/// above it with a `.git` (followed through symlinks; one that can't be
/// read is passed over), and whether that `.git` is a file, as a linked
/// worktree's is. `None` outside any checkout.
fn checkout_holding(dir: &Path) -> Option<(&Path, bool)> {
    dir.ancestors().find_map(|a| {
        std::fs::metadata(a.join(".git"))
            .ok()
            .map(|m| (a, m.is_file()))
    })
}

/// The nearest ancestor strictly above `top` whose `repos.toml` is the
/// same file as `registry`, or `None` when there's none or `registry`
/// can't be read.
fn nearest_link_above(top: &Path, registry: &Path) -> Option<PathBuf> {
    let registry = file_id(registry)?;
    top.ancestors()
        .skip(1)
        .find(|a| file_id(&a.join(REGISTRY_FILE)) == Some(registry))
        .map(Path::to_owned)
}

/// The device and inode of the file at `path`, through symlinks, or `None`
/// on any error.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// When `dir` is in a linked worktree: the nearest link above the repo's
/// main checkout to the `repos.toml` there at `dir`'s relative path, or
/// `None`. `dir` is physical (`find_registry`), as git's paths are.
fn linked_from_main_checkout(dir: &Path, git: &Git) -> Option<PathBuf> {
    let found = main_checkout(dir, true, git)?;
    let relative = dir.strip_prefix(found.toplevel?).ok()?;
    nearest_link_above(&found.main, &found.main.join(relative).join(REGISTRY_FILE))
}

/// A linked worktree's repo, as `main_checkout` finds it.
struct MainCheckout {
    main: PathBuf,
    /// The linked worktree's top level, when asked for.
    toplevel: Option<PathBuf>,
}

/// The main checkout of the linked worktree `cwd` is in, and with
/// `toplevel` the worktree's top level, or `None` when `cwd` isn't in a
/// linked worktree, or on any git failure (the caller reports the registry
/// missing, or keeps it where it was found) — `toplevel` fails in a git
/// dir.
///
/// A linked worktree's git dir differs from its common dir; anywhere else
/// they're the same dir — a main checkout, a submodule, or a checkout whose
/// git dir lives elsewhere (`--separate-git-dir`), where the common dir's
/// parent is no checkout at all and must not be searched. The main checkout
/// is the common dir's parent only when the common dir is named `.git`: one
/// kept elsewhere (a linked worktree of a `--separate-git-dir` repo) says
/// nothing of where its main checkout is, unless it's named `.git` — then
/// its parent is taken as the main checkout, as `git worktree list` does.
///
/// No ceiling: git's own discovery walks up from `cwd` to the worktree (the
/// cwd may be deep inside it), which a ceiling would cut short, and only the
/// indirection above could point the search astray.
fn main_checkout(cwd: &Path, toplevel: bool, git: &Git) -> Option<MainCheckout> {
    let mut args = vec![
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ];
    if toplevel {
        args.push("--show-toplevel");
    }
    let out = git.output_string(cwd, &args, CallOptions::default()).ok()?;
    let mut lines = out.lines();
    let (git_dir, common) = (Path::new(lines.next()?), Path::new(lines.next()?));
    if git_dir == common || common.file_name()? != ".git" {
        return None;
    }
    let toplevel = if toplevel {
        Some(PathBuf::from(lines.next()?))
    } else {
        None
    };
    Some(MainCheckout {
        main: common.parent()?.to_owned(),
        toplevel,
    })
}

/// Refuses a discovered root (`RegistryLocation::discovered`) that is at or
/// inside a checkout of one of the registry's own entries.
///
/// That's the registry's repo cloned with no link at the workspace root
/// yet, a symlinked entry dir walked physically, a worktree whose own copy
/// of the registry won because the main checkout's is gone, or a bare
/// repo's worktree. Rooted there, every other entry would read as missing,
/// and a sync would clone them into it.
///
/// Read without git unless the root is in a checkout (the nearest `.git`
/// at or above it); then git reads that checkout's `origin` URLs both as
/// configured (`config --get-all remote.origin.url`) and as git resolves
/// them (`remote get-url --all`: `insteadOf` applied, worktree config
/// honored), and any of either naming an entry as `origin_matches` reads
/// an origin — owned and third-party entries alike — refuses. Both, since
/// a rewrite can hide the repo either way: an alias (`gh:o/meta`) names it
/// only once resolved, and a rewrite to a mirror or a local path only as
/// configured. The resolved read is skipped when the configured one
/// matches. A checkout of no entry (a dotfiles repo further out, a
/// workspace that is itself an unlisted repo) passes, as does any git
/// failure: this is a backstop, not the rule.
///
/// # Errors
///
/// `RootInEntry` naming the first such entry, in registry order.
fn check_discovered_root(loc: &RegistryLocation, entries: &[Entry], git: &Git) -> Result<()> {
    if !loc.discovered {
        return Ok(());
    }
    let Some((top, _)) = checkout_holding(&loc.root) else {
        return Ok(());
    };
    // the first entry, in registry order, one of the URLs git prints names
    let named = |args: &[&str]| {
        let out = git.output_string(top, args, CallOptions::default()).ok()?;
        entries
            .iter()
            .find(|e| out.lines().any(|origin| origin_matches(origin, &e.url)))
    };
    let entry = named(&["config", "--get-all", "remote.origin.url"])
        .or_else(|| named(&["remote", "get-url", "--all", "origin"]));
    entry.map_or(Ok(()), |e| {
        Err(Error::RootInEntry {
            root: loc.root.clone(),
            registry: loc.path.clone(),
            key: e.key.clone(),
        })
    })
}

/// Selects the entries `targets` name, in registry order.
///
/// No targets selects every entry. A target is a registry key, else an
/// entry's dir name, else a path (relative to `cwd`) inside a checkout —
/// the entry whose own checkout it is, else resolved through its git common
/// dir, so a linked worktree outside the workspace resolves too
/// (`entry_of_repo`).
///
/// # Errors
///
/// `UnknownEntry` for a target that matches nothing; `GitNotFound` when a
/// path target can't be resolved for lack of git.
pub fn resolve_targets(
    entries: &[Entry],
    root: &Path,
    cwd: &Path,
    targets: &[String],
    git: &Git,
) -> Result<Vec<Entry>> {
    if targets.is_empty() {
        return Ok(entries.to_vec());
    }
    let mut selected = HashSet::new();
    for target in targets {
        let found = entries
            .iter()
            .position(|e| e.key == *target)
            .or_else(|| entries.iter().position(|e| e.dir == *target))
            .map_or_else(
                || resolve_path(entries, root, &cwd.join(target), git),
                |i| Ok(Some(i)),
            )?;
        let Some(i) = found else {
            return Err(Error::UnknownEntry {
                name: target.clone(),
                suggestions: suggest_keys(entries, target),
            });
        };
        selected.insert(i);
    }
    Ok(entries
        .iter()
        .enumerate()
        .filter(|(i, _)| selected.contains(i))
        .map(|(_, e)| e.clone())
        .collect())
}

/// How many close keys an unknown target suggests.
const MAX_SUGGESTIONS: usize = 3;

/// The registry keys close to `target`, best first. An entry is close when
/// its key or its dir name — both are targets — is within a small edit
/// distance of the target (a third of the longer name's length, at least one
/// edit), or one contains the other (at least three chars), compared
/// case-insensitively; it ranks by the closer of the two, then by key, and is
/// suggested by its key. A path-shaped target is compared by its last
/// component.
fn suggest_keys(entries: &[Entry], target: &str) -> Vec<String> {
    let name = Path::new(target)
        .components()
        .next_back()
        .and_then(|c| match c {
            std::path::Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .unwrap_or("")
        .to_lowercase();
    if name.is_empty() {
        return Vec::new();
    }
    // the distance to `candidate` when it's close, else `None`
    let score = |candidate: &str| {
        let candidate = candidate.to_lowercase();
        let distance = edit_distance(&name, &candidate);
        let longer = name.chars().count().max(candidate.chars().count());
        let near = distance <= (longer / 3).max(1);
        let contains =
            |outer: &str, inner: &str| inner.chars().count() >= 3 && outer.contains(inner);
        (near || contains(&candidate, &name) || contains(&name, &candidate)).then_some(distance)
    };
    let mut close: Vec<(usize, &str)> = entries
        .iter()
        .filter_map(|e| {
            let distance = [score(&e.key), score(&e.dir)].into_iter().flatten().min()?;
            Some((distance, e.key.as_str()))
        })
        .collect();
    close.sort_unstable();
    // a key in both tables (which validation rejects) is suggested once
    close.dedup_by(|a, b| a.1 == b.1);
    close
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, key)| key.to_owned())
        .collect()
}

/// The optimal-string-alignment distance: insertions, deletions,
/// substitutions, and transpositions of adjacent chars, each one edit.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // three rows: two back, one back, current
    let mut prev2 = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(prev2[j - 2] + 1);
            }
            cur[j] = d;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// A path resolved down to the checkout holding it (`resolve_checkout`).
#[derive(Debug, Clone)]
pub struct PathTarget {
    /// The entry the path names, as a path target names it.
    pub entry: Entry,
    /// The top level of the checkout holding the path, as git finds it
    /// from there: a linked worktree's own, not the entry's dir.
    pub checkout: PathBuf,
}

/// The entry whose checkout holds `path`, and that checkout's top level.
///
/// The entry is resolved as a path target's is (`entry_of_repo`: an
/// entry's own checkout, else through its git common dir, so a linked
/// worktree outside the workspace resolves too). `None` when `path` is in
/// no entry's checkout — at the workspace root, in an unregistered clone,
/// outside the workspace, or where git finds no work tree (inside a git
/// dir).
///
/// # Errors
///
/// `GitNotFound` when `path` can't be resolved for lack of git.
pub fn resolve_checkout(
    entries: &[Entry],
    root: &Path,
    path: &Path,
    git: &Git,
) -> Result<Option<PathTarget>> {
    let Some(out) = rev_parse(
        path,
        &["--git-dir", "--git-common-dir", "--show-toplevel"],
        git,
    )?
    else {
        return Ok(None);
    };
    let mut lines = out.lines();
    let (Some(git_dir), Some(common), Some(toplevel)) = (lines.next(), lines.next(), lines.next())
    else {
        return Ok(None);
    };
    let toplevel = Path::new(toplevel);
    let git_dir = Path::new(git_dir);
    // a work tree elsewhere (`core.worktree`) is another checkout's files:
    // no entry's checkout to name
    if !owns_git_dir(toplevel, git_dir) {
        return Ok(None);
    }
    let dirs = RepoDirs {
        git_dir,
        common: Path::new(common),
        toplevel: Some(toplevel),
    };
    Ok(entry_of_repo(entries, root, &dirs).map(|i| PathTarget {
        entry: entries[i].clone(),
        checkout: toplevel.to_owned(),
    }))
}

/// Resolves `repos push`'s targets to the checkouts they name, in the order
/// given, each checkout once.
///
/// No targets names the checkout holding `cwd` (`resolve_checkout`). A
/// target that's a registry key or an entry's dir name names that entry's
/// dir, its primary checkout; else it's a path (relative to `cwd`) naming
/// the checkout holding it — a linked worktree's own, wherever it is.
///
/// # Errors
///
/// `NoCheckout` without targets when `cwd` is in no entry's checkout;
/// `UnknownEntry` for a target that names nothing; `GitNotFound` when a
/// path can't be resolved for lack of git.
pub fn resolve_push_targets(
    entries: &[Entry],
    root: &Path,
    cwd: &Path,
    targets: &[String],
    git: &Git,
) -> Result<Vec<PathTarget>> {
    if targets.is_empty() {
        return resolve_checkout(entries, root, cwd, git)?
            .map(|t| vec![t])
            .ok_or_else(|| Error::NoCheckout {
                path: cwd.to_path_buf(),
            });
    }
    let mut resolved: Vec<PathTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        let named = entries
            .iter()
            .find(|e| e.key == *target)
            .or_else(|| entries.iter().find(|e| e.dir == *target));
        let found = match named {
            Some(e) => PathTarget {
                entry: e.clone(),
                checkout: root.join(&e.dir),
            },
            None => resolve_checkout(entries, root, &cwd.join(target), git)?.ok_or_else(|| {
                Error::UnknownEntry {
                    name: target.clone(),
                    suggestions: suggest_keys(entries, target),
                }
            })?,
        };
        // the same checkout named twice, by a key and a path, say
        let seen = resolved.iter().any(|t| {
            t.entry.key == found.entry.key
                && (t.checkout == found.checkout || same_canonical(&t.checkout, &found.checkout))
        });
        if !seen {
            resolved.push(found);
        }
    }
    Ok(resolved)
}

/// The entry whose checkout holds `path` (`entry_of_repo`). Inside a git
/// dir, where `--show-toplevel` fails, it's found without a top level.
fn resolve_path(entries: &[Entry], root: &Path, path: &Path, git: &Git) -> Result<Option<usize>> {
    let out = match rev_parse(
        path,
        &["--git-dir", "--git-common-dir", "--show-toplevel"],
        git,
    )? {
        Some(out) => out,
        None => match rev_parse(path, &["--git-dir", "--git-common-dir"], git)? {
            Some(out) => out,
            None => return Ok(None),
        },
    };
    let mut lines = out.lines();
    let (Some(git_dir), Some(common)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    let dirs = RepoDirs {
        git_dir: Path::new(git_dir),
        common: Path::new(common),
        toplevel: lines.next().map(Path::new),
    };
    Ok(entry_of_repo(entries, root, &dirs))
}

/// `git rev-parse --path-format=absolute` with `args`, run in `path`: its
/// stdout, or `None` when `path` doesn't exist or git fails there.
fn rev_parse(path: &Path, args: &[&str], git: &Git) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let args: Vec<&str> = ["rev-parse", "--path-format=absolute"]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    match git.output_string(path, &args, CallOptions::default()) {
        Ok(out) => Ok(Some(out)),
        Err(GitError::NotFound) => Err(Error::GitNotFound),
        Err(_) => Ok(None),
    }
}

/// Where git finds a path's repo, as `rev-parse --path-format=absolute`
/// prints it: `--git-dir`, `--git-common-dir`, and `--show-toplevel` when
/// the path has a work tree.
struct RepoDirs<'a> {
    git_dir: &'a Path,
    common: &'a Path,
    toplevel: Option<&'a Path>,
}

/// The entry `dirs` names, compared canonicalized: the first entry whose
/// dir is the checkout's top level, when that checkout's `.git` names the
/// path's git dir (`owns_git_dir`) — the checkout holding the path is an
/// entry's own, wherever its git dir is (`--separate-git-dir`, or a linked
/// worktree's), but a work tree a repo's `core.worktree` points at is
/// another checkout's files — else the dir holding the common dir, when
/// that's a `.git`: the main checkout of a linked worktree elsewhere, or of
/// a path inside a `.git`.
///
/// Outside the workspace, a linked worktree of a repo whose common dir
/// isn't a `.git` names no entry: of a `--separate-git-dir` repo, nothing
/// in its git dirs says where the main checkout is (git's own worktree list
/// prints the git dir in its place); of a bare repo, no entry's probe reads
/// one (it has no work tree).
fn entry_of_repo(entries: &[Entry], root: &Path, dirs: &RepoDirs<'_>) -> Option<usize> {
    let main_checkout = dirs
        .common
        .parent()
        .filter(|_| dirs.common.file_name().is_some_and(|n| n == ".git"));
    let entry_dirs: Vec<Option<PathBuf>> = entries
        .iter()
        .map(|e| root.join(&e.dir).canonicalize().ok())
        .collect();
    let own = dirs.toplevel.filter(|t| owns_git_dir(t, dirs.git_dir));
    [own, main_checkout]
        .into_iter()
        .flatten()
        .filter_map(|c| c.canonicalize().ok())
        .find_map(|repo| entry_dirs.iter().position(|d| d.as_ref() == Some(&repo)))
}

/// Whether `toplevel`'s own `.git` names `git_dir` (`dot_git_target`),
/// compared canonicalized: true of a main checkout, a linked worktree, and
/// a `--separate-git-dir` checkout; false of a work tree a repo's
/// `core.worktree` points at, whose `.git` is its own or none.
fn owns_git_dir(toplevel: &Path, git_dir: &Path) -> bool {
    let named = dot_git_target(&toplevel.join(".git")).ok();
    match (
        named.and_then(|d| d.canonicalize().ok()),
        git_dir.canonicalize(),
    ) {
        (Some(named), Ok(git_dir)) => named == git_dir,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
