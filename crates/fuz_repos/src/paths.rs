//! Path resolution the other modules share: canonicalizing (`canonical`,
//! `same_canonical`), and resolving a path that may not exist all the way —
//! as far as it exists (`realpath_forgiving`), or component by component as
//! the kernel would (`resolve`, `same_path`).

use std::path::{Component, Path, PathBuf};

/// A path canonicalized, or `None` when it can't be.
pub fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// Whether two paths are the same dir, compared canonicalized; `false` when
/// either can't be.
pub fn same_canonical(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// `path` with its longest existing prefix canonicalized and the rest
/// appended as is.
pub fn realpath_forgiving(path: &Path) -> PathBuf {
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

/// Where resolving a path stopped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub path: String,
    pub error: String,
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
///
/// # Errors
///
/// Where resolving stopped (`Unresolved`), as above.
pub fn resolve(path: &Path) -> Result<PathBuf, Unresolved> {
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

/// Whether `a` and `b` name one path once each is resolved as the kernel
/// would (`resolve`: missing components taken as written); `false` when
/// either can't be.
pub fn same_path(a: &Path, b: &Path) -> bool {
    matches!((resolve(a), resolve(b)), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests;
