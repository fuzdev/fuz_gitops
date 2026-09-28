//! Stamps the binary with the commit it was built from, for `repos --version`:
//! the short commit id, plus `, dirty` when the binary's sources have
//! uncommitted changes.

use std::path::Path;
use std::process::Command;

/// What the binary is built from: the dirty check and the rerun watch cover
/// the same paths, so the stamp can't go stale on an edit it counts.
const SOURCES: [&str; 3] = ["src", "build.rs", "Cargo.toml"];

fn main() {
    let stamp = git(&["rev-parse", "--short=12", "HEAD"]).map_or_else(
        || "unknown".to_owned(),
        |commit| {
            // scoped to the sources: dirt elsewhere in the repo doesn't
            // change the binary
            let mut status = vec!["status", "--porcelain", "--"];
            status.extend(SOURCES);
            if git(&status).is_some() {
                format!("{commit}, dirty")
            } else {
                commit
            }
        },
    );
    println!("cargo::rustc-env=REPOS_BUILD={stamp}");

    // rerun when the sources change (so the dirty stamp stays true) or HEAD
    // moves; never on index writes, which any `git status` makes
    for path in SOURCES {
        println!("cargo::rerun-if-changed={path}");
    }
    // `--git-path` maps each into the common dir from a linked worktree; a
    // missing path would rerun every build, so only existing ones are watched
    let watch = |path: &str| {
        let found = git(&["rev-parse", "--git-path", path]).filter(|p| Path::new(p).exists());
        if let Some(found) = &found {
            println!("cargo::rerun-if-changed={found}");
        }
        found.is_some()
    };
    watch("HEAD");
    // HEAD's reflog, appended on every move: a commit on a packed ref (after
    // `git gc`) creates a loose ref not watched yet and leaves `packed-refs`
    // alone, so only the reflog sees it
    watch("logs/HEAD");
    // the branch's ref itself, loose else packed, for a repo without reflogs
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"])
        && !watch(&head_ref)
    {
        watch("packed-refs");
    }
}

/// Runs git in the crate dir; `None` on failure or empty output.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}
