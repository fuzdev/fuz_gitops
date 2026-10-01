use super::*;

#[test]
fn paths_resolve_as_the_kernel_would_where_they_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let real = base.join("real");
    std::fs::create_dir_all(real.join("app/src")).unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    std::os::unix::fs::symlink(real.join("app"), real.join("app-link")).unwrap();
    let at = |p: &Path| resolve(p).unwrap();
    assert_eq!(at(&link.join("app/src")), real.join("app/src"));
    // a deleted dir under a symlink: the rest still resolves
    assert_eq!(at(&link.join("app/gone/x")), real.join("app/gone/x"));
    // `..` past a dir that doesn't exist undoes it, not the symlink
    assert_eq!(at(&link.join("gone/../app")), real.join("app"));
    assert_eq!(at(&link.join("app/../other")), real.join("other"));
    assert_eq!(at(&link.join("gone/./../app-link/y")), real.join("app/y"));
    // `..` out of a symlinked dir goes to its target's parent
    assert_eq!(at(&real.join("app-link/../gone")), real.join("gone"));
    assert_eq!(
        at(Path::new("/nonexistent-ws/a/../b")),
        Path::new("/nonexistent-ws/b")
    );
    // a name under a file: there's nothing there either
    std::fs::write(real.join("file"), "").unwrap();
    assert_eq!(at(&link.join("file/x/../y")), real.join("file/y"));
}

/// Whether `resolve` failed at `path`.
fn fails_at(got: Result<PathBuf, Unresolved>, path: &Path) -> bool {
    matches!(got, Err(Unresolved { path: p, .. }) if p == path.to_string_lossy())
}

#[test]
fn a_symlink_loop_fails_to_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(base.join("b"), base.join("a")).unwrap();
    std::os::unix::fs::symlink(base.join("a"), base.join("b")).unwrap();
    std::os::unix::fs::symlink(base.join("self"), base.join("self")).unwrap();
    for (path, at) in [
        (base.join("a/app/src"), base.join("a")),
        (base.join("self"), base.join("self")),
        (base.join("self/../x"), base.join("self")),
    ] {
        let got = resolve(&path);
        assert!(fails_at(got.clone(), &at), "{}: {got:?}", path.display());
    }
}

/// Restores a dir's mode when dropped, so a failed assertion doesn't
/// leave a tempdir that can't be removed.
struct Unlock<'a>(&'a Path);

impl Drop for Unlock<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_dir_that_cannot_be_searched_fails_to_resolve() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let locked = base.join("locked");
    std::fs::create_dir_all(locked.join("app/src")).unwrap();
    let cwd = locked.join("app/src");
    assert_eq!(resolve(&cwd).unwrap(), cwd);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _unlock = Unlock(&locked);
    // root searches it anyway, so there's nothing to test
    match std::fs::symlink_metadata(locked.join("app")) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
        other => {
            eprintln!(
                "skipped: {} is searchable at mode 0 ({other:?})",
                locked.display()
            );
            return;
        }
    }
    let got = resolve(&cwd);
    assert!(fails_at(got.clone(), &locked.join("app")), "{got:?}");
    // the dir itself resolves: its parent can be searched
    assert_eq!(resolve(&locked).unwrap(), locked);
}
