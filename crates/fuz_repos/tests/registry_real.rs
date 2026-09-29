//! Parses and validates the real workspace registry when present — the cheap
//! guard that this parser and validator and the registry's other validator
//! still agree. Skipped (passes vacuously) where no `~/dev/repos.toml`
//! exists, as in CI.

use std::path::PathBuf;

use fuz_repos::registry::{CheckoutMode, EntryKind, Registry};

#[test]
fn parses_and_validates_the_real_registry() {
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
    // every integrity rule holds, each issue listed when one doesn't
    let registry = registry.validate().unwrap_or_else(|issues| {
        let lines: Vec<String> = issues.iter().map(ToString::to_string).collect();
        panic!("the real registry is invalid:\n{}", lines.join("\n"))
    });
    let entries = registry.entries();
    assert!(entries.iter().any(|e| e.kind == EntryKind::Repo));
    assert!(entries.iter().any(|e| e.kind == EntryKind::Reference));
    // every repo follows a branch (dirs are unique: `DirClaimedTwice`)
    for e in &entries {
        if e.kind == EntryKind::Repo {
            assert!(
                matches!(e.checkout_mode, CheckoutMode::Follow { .. }),
                "{}",
                e.key
            );
        }
    }
}
