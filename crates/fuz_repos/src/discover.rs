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
/// The root is `root` if given (relative to `cwd`), for a registry kept
/// outside the workspace, else the registry's dir.
///
/// # Errors
///
/// `RegistryNotFound` when the walk-up reaches the filesystem root;
/// `RootNotFound` when `root` isn't a directory — a mistyped root would
/// otherwise read every entry as missing.
pub fn find_registry(
    cwd: &Path,
    explicit: Option<&Path>,
    root: Option<&Path>,
) -> Result<RegistryLocation> {
    let mut found = if let Some(explicit) = explicit {
        let path = cwd.join(explicit);
        let root = path.parent().map_or_else(|| cwd.to_owned(), Path::to_owned);
        RegistryLocation { path, root }
    } else {
        cwd.ancestors()
            .map(|dir| (dir, dir.join(REGISTRY_FILE)))
            .find(|(_, path)| path.is_file())
            .map(|(dir, path)| RegistryLocation {
                path,
                root: dir.to_owned(),
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
    }
    Ok(found)
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
            // TODO: suggest close keys (pass 2)
            return Err(Error::UnknownEntry {
                name: target.clone(),
                suggestions: Vec::new(),
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

        let found = find_registry(&nested, None, None).unwrap();
        assert_eq!(found.root, root);
        assert_eq!(found.path, root.join(REGISTRY_FILE));
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

        let found = find_registry(&ws.join("repo"), None, None).unwrap();
        assert_eq!(found.root, ws);
    }

    #[test]
    fn explicit_path_is_relative_to_cwd() {
        let found = find_registry(
            Path::new("/a/b"),
            Some(Path::new("../reg/repos.toml")),
            None,
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
        let found =
            find_registry(&ws, Some(&meta.join(REGISTRY_FILE)), Some(Path::new("."))).unwrap();
        assert_eq!(found.path, meta.join(REGISTRY_FILE));
        assert_eq!(found.root, ws.join("."));

        // with the walk-up
        let found = find_registry(&meta, None, Some(&ws)).unwrap();
        assert_eq!(found.path, meta.join(REGISTRY_FILE));
        assert_eq!(found.root, ws);
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(REGISTRY_FILE), "owners = []\n").unwrap();
        let e = find_registry(tmp.path(), None, Some(Path::new("nope"))).unwrap_err();
        assert!(matches!(e, Error::RootNotFound { .. }), "{e}");
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn not_found_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = find_registry(tmp.path(), None, None).unwrap_err();
        assert!(matches!(e, Error::RegistryNotFound { .. }));
    }
}
