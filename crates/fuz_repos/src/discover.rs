//! Finding the registry, the workspace root it names, and the entries a
//! command's targets select.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{CallOptions, Git, GitError};
use crate::registry::Entry;

/// The registry's file name, found by walking up from the cwd.
pub const REGISTRY_FILE: &str = "repos.toml";

/// Where the registry was found. `root` is the directory holding `path` as
/// found — never the target of a symlinked registry.
#[derive(Debug, Clone)]
pub struct RegistryLocation {
    pub path: PathBuf,
    pub root: PathBuf,
}

/// Locates the registry and the workspace root.
///
/// The registry is `explicit` if given (relative to `cwd`), else the first
/// `repos.toml` in `cwd` or an ancestor — no env var, no `$HOME` fallback.
/// When that walk finds nothing and `cwd` is inside a linked worktree, it
/// runs once more from the repo's main checkout, so a linked worktree outside
/// the workspace finds its repo's registry. The root
/// is `root` if given (relative to `cwd`), for a registry kept outside the
/// workspace, else the registry's dir.
///
/// # Errors
///
/// `RegistryNotFound` when neither walk finds a registry; `RootNotFound` when
/// `root` isn't a directory — a mistyped root would otherwise read every
/// entry as missing.
pub fn find_registry(
    cwd: &Path,
    explicit: Option<&Path>,
    root: Option<&Path>,
    git: &Git,
) -> Result<RegistryLocation> {
    let mut found = if let Some(explicit) = explicit {
        let path = cwd.join(explicit);
        let root = path.parent().map_or_else(|| cwd.to_owned(), Path::to_owned);
        RegistryLocation { path, root }
    } else {
        walk_up(cwd)
            .or_else(|| walk_up(&main_checkout(cwd, git)?))
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
    }
    Ok(found)
}

/// The first `repos.toml` in `start` or an ancestor.
fn walk_up(start: &Path) -> Option<RegistryLocation> {
    start
        .ancestors()
        .map(|dir| (dir, dir.join(REGISTRY_FILE)))
        .find(|(_, path)| path.is_file())
        .map(|(dir, path)| RegistryLocation {
            path,
            root: dir.to_owned(),
        })
}

/// The main checkout of the linked worktree `cwd` is in, or `None` when
/// `cwd` isn't in a linked worktree, or on any git failure (the caller
/// reports the registry missing).
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
fn main_checkout(cwd: &Path, git: &Git) -> Option<PathBuf> {
    let out = git
        .output_string(
            cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
            CallOptions::default(),
        )
        .ok()?;
    let mut lines = out.lines();
    let (git_dir, common) = (Path::new(lines.next()?), Path::new(lines.next()?));
    if git_dir == common || common.file_name()? != ".git" {
        return None;
    }
    common.parent().map(Path::to_owned)
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

/// The entry whose checkout holds `path`, compared canonicalized.
fn resolve_path(entries: &[Entry], root: &Path, path: &Path, git: &Git) -> Result<Option<usize>> {
    if !path.exists() {
        return Ok(None);
    }
    let common = match git.output_string(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        CallOptions::default(),
    ) {
        Ok(out) => PathBuf::from(out.trim()),
        Err(GitError::NotFound) => return Err(Error::GitNotFound),
        Err(_) => return Ok(None),
    };
    let Some(repo) = common.parent().and_then(|p| p.canonicalize().ok()) else {
        return Ok(None);
    };
    Ok(entries
        .iter()
        .position(|e| root.join(&e.dir).canonicalize().is_ok_and(|d| d == repo)))
}

#[cfg(test)]
mod tests {
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
