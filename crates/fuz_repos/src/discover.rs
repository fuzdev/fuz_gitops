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

/// Locates the registry: `explicit` if given (relative to `cwd`), else the
/// first `repos.toml` in `cwd` or an ancestor. No env var, no `$HOME`
/// fallback.
///
/// # Errors
///
/// `RegistryNotFound` when the walk-up reaches the filesystem root.
pub fn find_registry(cwd: &Path, explicit: Option<&Path>) -> Result<RegistryLocation> {
    if let Some(explicit) = explicit {
        let path = cwd.join(explicit);
        let root = path.parent().map_or_else(|| cwd.to_owned(), Path::to_owned);
        return Ok(RegistryLocation { path, root });
    }
    cwd.ancestors()
        .map(|dir| (dir, dir.join(REGISTRY_FILE)))
        .find(|(_, path)| path.is_file())
        .map(|(dir, path)| RegistryLocation {
            path,
            root: dir.to_owned(),
        })
        .ok_or_else(|| Error::RegistryNotFound {
            start: cwd.to_owned(),
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

        let found = find_registry(&nested, None).unwrap();
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

        let found = find_registry(&ws.join("repo"), None).unwrap();
        assert_eq!(found.root, ws);
    }

    #[test]
    fn explicit_path_is_relative_to_cwd() {
        let found = find_registry(Path::new("/a/b"), Some(Path::new("../reg/repos.toml"))).unwrap();
        assert_eq!(found.path, Path::new("/a/b/../reg/repos.toml"));
        assert_eq!(found.root, Path::new("/a/b/../reg"));
    }

    #[test]
    fn not_found_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let e = find_registry(tmp.path(), None).unwrap_err();
        assert!(matches!(e, Error::RegistryNotFound { .. }));
    }
}
