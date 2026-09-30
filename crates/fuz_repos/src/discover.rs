//! Finding the registry, the workspace root it names, and the entries a
//! command's targets select.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::classify::origin_matches;
use crate::error::{Error, Result};
use crate::git::{CallOptions, Git, GitError};
use crate::registry::Entry;

/// The registry's file name, found by walking up from the cwd.
pub const REGISTRY_FILE: &str = "repos.toml";

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
pub fn check_discovered_root(loc: &RegistryLocation, entries: &[Entry], git: &Git) -> Result<()> {
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
/// resolved through its git common dir, so a linked worktree outside the
/// workspace resolves too.
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
/// The entry is resolved as a path target's is (`resolve_targets`: through
/// its git common dir, so a linked worktree outside the workspace resolves
/// too). `None` when `path` is in no entry's checkout — at the workspace
/// root, in an unregistered clone, outside the workspace, or where git
/// finds no work tree (inside a git dir).
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
    let Some(out) = rev_parse(path, &["--git-common-dir", "--show-toplevel"], git)? else {
        return Ok(None);
    };
    let mut lines = out.lines();
    let (Some(common), Some(toplevel)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    Ok(
        entry_of_common_dir(entries, root, Path::new(common)).map(|i| PathTarget {
            entry: entries[i].clone(),
            checkout: PathBuf::from(toplevel),
        }),
    )
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
                && (t.checkout == found.checkout || same_checkout(&t.checkout, &found.checkout))
        });
        if !seen {
            resolved.push(found);
        }
    }
    Ok(resolved)
}

/// Whether two checkout paths are the same dir, compared canonicalized.
fn same_checkout(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// The entry whose checkout holds `path`, compared canonicalized.
fn resolve_path(entries: &[Entry], root: &Path, path: &Path, git: &Git) -> Result<Option<usize>> {
    Ok(rev_parse(path, &["--git-common-dir"], git)?
        .and_then(|common| entry_of_common_dir(entries, root, Path::new(common.trim()))))
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

/// The entry whose dir is the repo of the git common dir `common` — the
/// dir holding it — compared canonicalized.
fn entry_of_common_dir(entries: &[Entry], root: &Path, common: &Path) -> Option<usize> {
    let repo = common.parent().and_then(|p| p.canonicalize().ok())?;
    entries
        .iter()
        .position(|e| root.join(&e.dir).canonicalize().is_ok_and(|d| d == repo))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn walks_up_to_the_first_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        let nested = root.join("repo/src/lib");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(REGISTRY_FILE), "owners = []\n").unwrap();

        let git = Git::new();
        let found = find_registry(&nested, None, None, &git).unwrap();
        assert_eq!(found.root, root);
        assert_eq!(found.path, root.join(REGISTRY_FILE));
        // the fallback through git runs only when the walk finds nothing
        assert_eq!(git.spawns(), 0);
    }

    #[test]
    fn a_symlinked_registry_roots_at_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = tmp.path().join("meta");
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::create_dir_all(ws.join("repo")).unwrap();
        std::fs::write(meta.join(REGISTRY_FILE), "owners = []\n").unwrap();
        std::os::unix::fs::symlink(meta.join(REGISTRY_FILE), ws.join(REGISTRY_FILE)).unwrap();

        let found = find_registry(&ws.join("repo"), None, None, &Git::new()).unwrap();
        assert_eq!(found.root, ws);
    }

    /// Writes `dir/repos.toml` — the registry for real, or a symlink to
    /// `target` — creating `dir`.
    fn place(dir: &Path, target: Option<&Path>) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(REGISTRY_FILE);
        match target {
            Some(target) => std::os::unix::fs::symlink(target, &path).unwrap(),
            None => std::fs::write(&path, "owners = []\n").unwrap(),
        }
        path
    }

    /// Makes `dir` look like a main checkout to discovery: a `.git` dir.
    fn checkout(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn a_registry_in_a_checkout_roots_at_the_nearest_link_above_it() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path();
        let ws = top.join("ws");
        let meta = ws.join("meta");
        // kept in `meta` under `reg/`, linked inside `meta` too; the root's
        // link names that one, and a stray link further out names the root's
        let real = place(&meta.join("reg"), None);
        let inside = place(&meta, Some(Path::new("reg/repos.toml")));
        let link = place(&ws, Some(&inside));
        place(top, Some(&link));
        checkout(&meta);
        for path in [&inside, &link, &top.join(REGISTRY_FILE)] {
            assert_eq!(path.canonicalize().unwrap(), real);
        }
        let deep = meta.join("reg/src/lib");
        std::fs::create_dir_all(&deep).unwrap();

        let git = Git::new();
        for start in [&deep, &meta.join("reg"), &meta, &ws] {
            let found = find_registry(start, None, None, &git).unwrap();
            assert_eq!(found.root, ws, "from {}", start.display());
            assert_eq!(found.path, link);
            assert!(found.discovered);
        }
        // outside any checkout, the first found is the root
        let found = find_registry(top, None, None, &git).unwrap();
        assert_eq!(found.root, top);
        // a `.git` dir: no spawn
        assert_eq!(git.spawns(), 0);

        // not a checkout: `meta`'s own links are where it's found
        std::fs::remove_dir(meta.join(".git")).unwrap();
        let found = find_registry(&deep, None, None, &git).unwrap();
        assert_eq!(found.root, meta.join("reg"));
        let found = find_registry(&meta, None, None, &git).unwrap();
        assert_eq!(found.root, meta);

        // a hard link is the same file too
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let real = place(&ws.join("meta"), None);
        checkout(&ws.join("meta"));
        std::fs::hard_link(&real, ws.join(REGISTRY_FILE)).unwrap();
        let found = find_registry(&ws.join("meta"), None, None, &git).unwrap();
        assert_eq!(found.root, ws);
    }

    #[test]
    fn a_different_registry_further_out_never_captures() {
        let tmp = tempfile::tempdir().unwrap();
        // an outer workspace with a registry of its own, and an inner one
        // whose registry the outer's copies
        let top = tmp.path();
        let outer = top.join("outer");
        let inner = outer.join("inner");
        let real = place(&inner, None);
        std::fs::copy(&real, outer.join(REGISTRY_FILE)).unwrap();
        assert_ne!(file_id(&real), file_id(&outer.join(REGISTRY_FILE)));

        let git = Git::new();
        let found = find_registry(&inner, None, None, &git).unwrap();
        assert_eq!(found.root, inner);
        assert_eq!(found.path, real);
        checkout(&inner);
        let found = find_registry(&inner, None, None, &git).unwrap();
        assert_eq!(found.root, inner);

        // nor does it stop the search: a link to the inner one further out
        // still roots there
        place(top, Some(&real));
        let found = find_registry(&inner, None, None, &git).unwrap();
        assert_eq!(found.root, top);
        assert_eq!(found.path, top.join(REGISTRY_FILE));
        // and from the outer workspace, its own registry is the one
        let found = find_registry(&outer, None, None, &git).unwrap();
        assert_eq!(found.root, outer);
    }

    #[test]
    fn an_ancestor_registry_that_cannot_be_read_is_passed_over() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path();
        let ws = top.join("ws");
        let meta = ws.join("meta");
        let real = place(&meta, None);
        place(top, Some(&real));
        // between them: a dangling link, a loop, and a link into a dir that
        // can't be searched
        let dangling = place(&ws, Some(&top.join("nowhere")));
        let looped = ws.join("loop");
        place(&looped, Some(&looped.join(REGISTRY_FILE)));
        let sealed = top.join("sealed");
        place(&sealed, Some(&real));
        let blocked = ws.join("blocked");
        place(&blocked, Some(&sealed.join(REGISTRY_FILE)));
        let unreadable = [
            dangling,
            looped.join(REGISTRY_FILE),
            blocked.join(REGISTRY_FILE),
        ];

        let starts = [
            meta.clone(),
            place(&looped.join("meta"), Some(&real)),
            place(&blocked.join("meta"), Some(&real)),
        ]
        .map(|path| path.parent().unwrap().to_owned());
        for start in &starts {
            checkout(start);
        }

        let git = Git::new();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable: Vec<bool> = unreadable
            .iter()
            .map(|path| std::fs::metadata(path).is_ok())
            .collect();
        let found = starts
            .clone()
            .map(|start| find_registry(&start, None, None, &git).unwrap());
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
        // the superuser searches any dir: the sealed one reads then, the same
        // file
        assert!(matches!(readable[..], [false, false, _]), "{readable:?}");
        for (start, found) in starts.iter().zip(found) {
            assert_eq!(found.root, top, "from {}", start.display());
            assert_eq!(found.path, top.join(REGISTRY_FILE));
        }

        // with nothing further out, the registry stays where it's found
        std::fs::remove_file(top.join(REGISTRY_FILE)).unwrap();
        let found = find_registry(&meta, None, None, &git).unwrap();
        assert_eq!(found.root, meta);
    }

    #[test]
    fn the_walk_is_over_the_physical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let ws = top.join("ws");
        let meta = ws.join("meta");
        let real = place(&meta, None);
        checkout(&meta);
        std::fs::create_dir(ws.join("app")).unwrap();
        // `decoy/..` is `ws` to the kernel, `elsewhere` read lexically, where
        // a stray link names the registry
        let elsewhere = top.join("elsewhere");
        place(&elsewhere, Some(&real));
        std::os::unix::fs::symlink(ws.join("app"), elsewhere.join("decoy")).unwrap();
        let dotted = elsewhere.join("decoy/../meta");
        assert_eq!(dotted.canonicalize().unwrap(), meta);

        let git = Git::new();
        let found = find_registry(&dotted, None, None, &git).unwrap();
        assert_eq!(found.root, meta);
        place(&ws, Some(&real));
        let found = find_registry(&dotted, None, None, &git).unwrap();
        assert_eq!(found.root, ws);
        assert_eq!(found.path, ws.join(REGISTRY_FILE));
        // a start that doesn't exist finds nothing
        let e = find_registry(&elsewhere.join("decoy/../nope"), None, None, &git).unwrap_err();
        assert!(matches!(e, Error::RegistryNotFound { .. }), "{e}");
    }

    #[test]
    fn git_is_asked_only_where_a_linked_worktree_may_hold_the_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        place(&repo, None);
        let deep = repo.join("src");
        std::fs::create_dir(&deep).unwrap();

        // no `.git` above, or a dir as a main checkout's is: no spawn
        let git = Git::new();
        assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
        checkout(&repo);
        assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
        assert_eq!(git.spawns(), 0);

        // a `.git` file, as a linked worktree's is: git decides, and a
        // failure keeps the registry where it's found
        std::fs::remove_dir(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: nowhere\n").unwrap();
        assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
        assert_eq!(git.spawns(), 1);
    }

    #[test]
    fn a_discovered_root_is_checked_only_inside_a_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        place(&ws, None);
        let es = entries(&["meta"]);
        let git = Git::new();
        // outside any checkout: no spawn
        let found = find_registry(&ws, None, None, &git).unwrap();
        check_discovered_root(&found, &es, &git).unwrap();
        assert_eq!(git.spawns(), 0);
        // named by `--root` or `--registry`: never checked
        checkout(&ws);
        let root = find_registry(&ws, None, Some(&ws), &git).unwrap();
        let explicit = find_registry(&ws, Some(Path::new("repos.toml")), None, &git).unwrap();
        for loc in [&root, &explicit] {
            assert!(!loc.discovered);
            check_discovered_root(loc, &es, &git).unwrap();
        }
        assert_eq!(git.spawns(), 0);
        // discovered in one: git reads its origin, as configured and as
        // resolved — here there's none
        let found = find_registry(&ws, None, None, &git).unwrap();
        check_discovered_root(&found, &es, &git).unwrap();
        assert_eq!(git.spawns(), 2);
    }

    #[test]
    fn an_explicit_registry_roots_at_its_dir_links_or_not() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let real = place(&ws.join("meta"), None);
        place(&ws, Some(&real));
        let found =
            find_registry(&ws, Some(Path::new("meta/repos.toml")), None, &Git::new()).unwrap();
        assert_eq!(found.root, ws.join("meta"));
        assert_eq!(found.path, real);
    }

    #[test]
    fn explicit_path_is_relative_to_cwd() {
        let found = find_registry(
            Path::new("/a/b"),
            Some(Path::new("../reg/repos.toml")),
            None,
            &Git::new(),
        )
        .unwrap();
        assert_eq!(found.path, Path::new("/a/b/../reg/repos.toml"));
        assert_eq!(found.root, Path::new("/a/b/../reg"));
    }

    #[test]
    fn root_overrides_the_registry_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let meta = tmp.path().join("meta");
        std::fs::create_dir_all(ws.join("repo")).unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join(REGISTRY_FILE), "owners = []\n").unwrap();

        // with an explicit registry
        let found = find_registry(
            &ws,
            Some(&meta.join(REGISTRY_FILE)),
            Some(Path::new(".")),
            &Git::new(),
        )
        .unwrap();
        assert_eq!(found.path, meta.join(REGISTRY_FILE));
        assert_eq!(found.root, ws.join("."));

        // with the walk-up
        let found = find_registry(&meta, None, Some(&ws), &Git::new()).unwrap();
        assert_eq!(found.path, meta.join(REGISTRY_FILE));
        assert_eq!(found.root, ws);
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(REGISTRY_FILE), "owners = []\n").unwrap();
        let e = find_registry(tmp.path(), None, Some(Path::new("nope")), &Git::new()).unwrap_err();
        assert!(matches!(e, Error::RootNotFound { .. }), "{e}");
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn not_found_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let git = Git::new();
        let e = find_registry(tmp.path(), None, None, &git).unwrap_err();
        assert!(matches!(e, Error::RegistryNotFound { .. }));
        assert_eq!(git.spawns(), 1);
        assert!(e.hint().is_some_and(|h| h.contains("--registry")), "{e}");
    }

    #[test]
    fn edit_distance_counts_each_edit_once() {
        for (a, b, d) in [
            ("", "", 0),
            ("gro", "gro", 0),
            ("gro", "", 3),
            ("gro", "gor", 1),  // transposition
            ("gro", "grp", 1),  // substitution
            ("gro", "groo", 1), // insertion
            ("fuz_ui", "fuz_css", 3),
            ("kitten", "sitting", 3),
            ("ca", "abc", 3),  // optimal string alignment, not full Damerau
            ("zzz", "żzz", 1), // chars, not bytes
        ] {
            assert_eq!(edit_distance(a, b), d, "{a} / {b}");
            assert_eq!(edit_distance(b, a), d, "{b} / {a}");
        }
    }

    /// Entries keyed `keys`, each in the dir of its name; `key:dir` names
    /// another dir.
    fn entries(keys: &[&str]) -> Vec<Entry> {
        use std::fmt::Write as _;
        let mut toml = String::from("owners = [\"me\"]\n");
        for k in keys {
            let (k, dir) = k.split_once(':').unwrap_or((k, k));
            let _ = write!(
                toml,
                "[repos.{k}]\nurl = \"https://github.com/me/{k}\"\nvisibility = \"public\"\n\
                 purpose = \"x\"\ndir = \"{dir}\"\n"
            );
        }
        crate::registry::Registry::parse(&toml)
            .unwrap()
            .validate()
            .unwrap()
            .entries()
    }

    #[test]
    fn suggests_close_keys_best_first() {
        let es = entries(&[
            "fuz_app", "fuz_css", "fuz_ui", "fuz_util", "gro", "grimoire", "tsv", "zzz", "FuzDocs",
            "cm",
        ]);
        let suggest = |t: &str| suggest_keys(&es, t);
        // a typo, a transposition, a case slip
        assert_eq!(suggest("gor"), ["gro"]);
        assert_eq!(suggest("fzu_ui"), ["fuz_ui"]);
        assert_eq!(suggest("GRO"), ["gro"]);
        // the key's case ignored too, and suggested as declared
        assert_eq!(suggest("fuzdoc"), ["FuzDocs"]);
        // near ones by distance, then key; capped
        assert_eq!(suggest("fuz_u"), ["fuz_ui", "fuz_util"]);
        assert_eq!(suggest("fuz"), ["fuz_ui", "FuzDocs", "fuz_app"]);
        // a prefix too far to be a typo, and a key inside the target
        assert_eq!(suggest("grim"), ["grimoire"]);
        assert_eq!(suggest("gro-old"), ["gro"]);
        // a path's last component
        assert_eq!(suggest("../tsb/"), ["tsv"]);
        assert_eq!(suggest("/elsewhere/zzz"), ["zzz"]);
        // a 2-char name still allows one edit
        assert_eq!(suggest("cn"), ["cm"]);
        // containment needs three chars: `fu` is in every `fuz_*`, and too
        // far from each by edits
        assert!(suggest("fu").is_empty());
        // nothing close
        assert!(suggest("mageguild").is_empty());
        assert!(suggest(".").is_empty());
        assert!(suggest("..").is_empty());
        assert!(suggest("").is_empty());
        // short names match short keys only by edits, never by containment
        assert!(suggest("z").is_empty());
        assert_eq!(suggest("zz"), ["zzz"]);
    }

    #[test]
    fn suggests_by_key_or_dir_named_by_key() {
        let es = entries(&[
            "fuz_forge:private_fuz_forge",
            "fuz_os:private_fuz_os",
            "tsv",
        ]);
        let suggest = |t: &str| suggest_keys(&es, t);
        // near the dir by one edit, and `fuz_os`'s dir by three
        assert_eq!(suggest("private_fuz_forg"), ["fuz_forge", "fuz_os"]);
        // the closer of key and dir ranks it: `fuz_os` by its key
        assert_eq!(suggest("fuz_o"), ["fuz_os"]);
        assert_eq!(suggest("../private_fuz_os/"), ["fuz_os", "fuz_forge"]);
    }
}
