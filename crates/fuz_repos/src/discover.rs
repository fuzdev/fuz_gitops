//! Finding the registry and the workspace root it names.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

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
