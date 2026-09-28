//! Parses the real workspace registry when present — the cheap guard that this
//! parser and the registry's other validator still agree. Skipped (passes
//! vacuously) where no `~/dev/repos.toml` exists, as in CI.

use std::path::PathBuf;

use fuz_repos::registry::{CheckoutMode, EntryKind, Registry};

#[test]
fn parses_the_real_registry() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = PathBuf::from(home).join("dev/repos.toml");
    if !path.is_file() {
        eprintln!("skipped: no registry at {}", path.display());
        return;
    }
    let registry = Registry::load(&path).unwrap_or_else(|e| panic!("{e}"));
    assert!(!registry.owners.is_empty());
    let entries = registry.entries();
    assert!(entries.iter().any(|e| e.kind == EntryKind::Repo));
    assert!(entries.iter().any(|e| e.kind == EntryKind::Reference));
    // every repo follows a branch; dirs are unique
    for e in &entries {
        if e.kind == EntryKind::Repo {
            assert!(
                matches!(e.checkout_mode, CheckoutMode::Follow { .. }),
                "{}",
                e.key
            );
        }
    }
    let mut dirs: Vec<_> = entries.iter().map(|e| e.dir.as_str()).collect();
    dirs.sort_unstable();
    let before = dirs.len();
    dirs.dedup();
    assert_eq!(before, dirs.len(), "a dir is claimed twice");
}
