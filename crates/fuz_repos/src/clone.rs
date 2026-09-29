//! Cloning a missing entry: `sync`'s one action on an entry as a whole,
//! carrying out its `CloneVerdict`.
//!
//! **The recipe** (`CloneRecipe`, decided in `classify`) becomes one
//! `git clone`:
//!
//! ```text
//! git -c transfer.bundleURI=false clone --quiet --no-tags
//!     --no-recurse-submodules --origin=origin [--branch=<b>] [--depth=1]
//!     [--filter=blob:none --sparse] --end-of-options <url> <temp>
//! ```
//!
//! - **From `Entry::remote_url`**: an owned entry's SSH URL
//!   (`git@<host>:<account>/<name>`), a third-party one's HTTPS URL — the
//!   `origin` the probe then expects, so a fresh clone reads no origin drift.
//!   `GIT_ALLOW_PROTOCOL` is that transport alone (`ssh` or `https`), so no
//!   `insteadOf` can send the clone, or the lazy fetch a sparse checkout
//!   makes, anywhere else; SSH runs in batch mode, as the fetch's does,
//!   unless the user configures it (`core.sshCommand`, `GIT_SSH_COMMAND`).
//! - **`--branch`** when the entry names one (a repo always does), so HEAD
//!   lands there with origin's branch of the name as its upstream; without
//!   one, the remote's default. A pin without a branch is left there for its
//!   consumer to align.
//! - **`--depth=1`** for a shallow reference, which implies
//!   `--single-branch`: origin's fetch refspec maps just the branch cloned,
//!   as the tool's own shallow fetches expect.
//! - **`--filter=blob:none --sparse`** for a sparse reference, then `git
//!   sparse-checkout set --cone --end-of-options <path>`: the clone checks
//!   out the top-level files alone and the set adds the one subtree,
//!   fetching only the blobs those need, so the whole tree is never
//!   materialized. Both calls may fetch missing objects on demand
//!   (`CallOptions::lazy_fetch`), from origin, over the same transport.
//! - **`--no-tags`**: as the tool's fetches bring none, the clone brings
//!   none. Git records that as `remote.origin.tagOpt=--no-tags`, which the
//!   clone then unsets, in the temp dir before it's placed: the tool's own
//!   fetches pass `--no-tags` anyway, and the user's `git fetch` there
//!   follows tags as it would in any clone made by hand (a publish's
//!   tags, a pin aligned to a tag).
//! - **Never submodules** (`--no-recurse-submodules`), and no bundle URIs a
//!   server advertises (`transfer.bundleURI=false`, for this call only).
//! - **No hooks run.** The runner's `core.hooksPath=/dev/null` reaches the
//!   clone and every git it spawns, so a `post-checkout` or
//!   `reference-transaction` hook — the user's template's, copied into the
//!   new repo as any clone copies it — never runs during the clone.
//!   Filter drivers the user's config defines are theirs, and run as in any
//!   checkout (the `git` module doc).
//!
//! **Nothing half-made lands at the entry's path.** Git clones into a temp
//! dir beside it, `<root>/.<dir>.repos-clone-<pid>-<nonce>` (`temp_dir`):
//! the nonce, random per process, keeps two runs apart whose pids match
//! (each in its own pid namespace, as sandboxed agents are). It's created
//! first (so a leftover is never reused: one there already fails the
//! clone, naming it), under `CLONE_TIMEOUT` — a timed-out git is stopped
//! with `SIGTERM`, as any call. Any failure deletes the temp dir; one a
//! killed run leaves behind, the unregistered scan names as the tool's
//! (`is_temp_dir_name`) — as it names one a running sync is still
//! cloning into, which it can't tell apart.
//! On success the clone moves into place: the path is claimed by creating
//! it as an empty dir, which fails when anything is there — a dir, a file,
//! a symlink, dangling or not — and then the temp dir is renamed over that
//! empty dir. Something made at the path in the instant between is never
//! overwritten: the claim fails (`Held` `changed`), and a file put in the
//! claimed dir fails the rename.
//!
//! **Read back in place**, as `repos status` reads it: present, HEAD on the
//! recipe's branch (any branch without one), that branch's upstream
//! origin's branch of the name, the checkout clean. A clone that doesn't
//! read so is reported failed and stays where it is, for a person to look
//! at.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::probe::{ProbeContext, Probed, RegistryDirs, RepoFetches, probe};
use crate::registry::Entry;
use crate::remote::{RefspecContext, RemoteFailure};
use crate::report::{CloneOutcome, SyncHold};
use crate::state::{CloneRecipe, Head};

/// The timeout for a clone and its sparse checkout, in place of
/// `NETWORK_TIMEOUT`.
///
/// A first clone of a large repo downloads its history and writes its
/// whole tree. Long enough for one making progress on a slow
/// link; a stalled one still ends.
pub const CLONE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The flags before the recipe's own (the module doc says why each).
const CLONE_ARGS: [&str; 7] = [
    "-c",
    "transfer.bundleURI=false",
    "clone",
    "--quiet",
    "--no-tags",
    "--no-recurse-submodules",
    "--origin=origin",
];

/// What cloning needs: the runner, the workspace root the entry's dir is
/// under, the whole registry's dirs (for the read back), and the timeout.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cloner<'a> {
    pub git: &'a Git,
    pub root: &'a Path,
    pub registry_dirs: &'a RegistryDirs,
    pub timeout: Duration,
}

impl Cloner<'_> {
    /// Clones `entry` by `recipe` into its missing dir, unless `busy_now`,
    /// asked right before, finds a live session at or under the path.
    pub(crate) fn clone_entry(
        &self,
        entry: &Entry,
        recipe: &CloneRecipe,
        busy_now: impl FnOnce(&Path) -> bool,
    ) -> CloneOutcome {
        let target = self.root.join(&entry.dir);
        if busy_now(&target) {
            return CloneOutcome::Held { by: SyncHold::Busy };
        }
        match std::fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // made since the probe read it missing
            Ok(_) => {
                return CloneOutcome::Held {
                    by: SyncHold::Changed,
                };
            }
            Err(e) => {
                return CloneOutcome::Failed {
                    message: format!("can't look up {}: {e}", target.display()),
                };
            }
        }
        let temp = temp_dir(self.root, &entry.dir);
        if let Err(e) = std::fs::create_dir(&temp) {
            let message = if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "the temp dir {} is already there, a clone repos didn't finish, or one \
                     still running: remove it once no `repos sync` is running, then rerun",
                    temp.display()
                )
            } else {
                format!("can't make the temp dir {}: {e}", temp.display())
            };
            return CloneOutcome::Failed { message };
        }
        if let Err(outcome) = self
            .clone_into(entry, recipe, &temp)
            .and_then(|()| place(&temp, &target))
        {
            return discard(&temp, outcome);
        }
        self.read_back(entry, recipe, &target)
    }

    /// Runs the recipe's clone into `temp`, then its sparse checkout, then
    /// unsets the `tagOpt` the clone's `--no-tags` recorded.
    fn clone_into(
        &self,
        entry: &Entry,
        recipe: &CloneRecipe,
        temp: &Path,
    ) -> Result<(), CloneOutcome> {
        let temp_s = temp.to_str().ok_or_else(|| CloneOutcome::Failed {
            message: format!("non-UTF-8 path {}", temp.display()),
        })?;
        // transport follows write authority, as `remote_url` does
        let (transport, batch_ssh) = if entry.writable {
            (
                "ssh",
                !self.git.env_configures_ssh() && !self.configures_ssh(temp),
            )
        } else {
            ("https", false)
        };
        let opts = CallOptions {
            ceiling: Some(self.root),
            network: Some(NetworkOptions { batch_ssh }),
            allow_protocol: Some(transport),
            timeout: Some(self.timeout),
            lazy_fetch: recipe.sparse.is_some(),
        };
        let branch = recipe.branch.as_ref().map(|b| format!("--branch={b}"));
        let mut args = CLONE_ARGS.to_vec();
        args.extend(branch.as_deref());
        if recipe.shallow {
            args.push("--depth=1");
        }
        if recipe.sparse.is_some() {
            args.extend(["--filter=blob:none", "--sparse"]);
        }
        args.extend(["--end-of-options", &recipe.url, temp_s]);
        self.git
            .output(self.root, &args, opts)
            .map_err(clone_failed)?;
        if let Some(path) = &recipe.sparse {
            self.git
                .output(
                    temp,
                    &["sparse-checkout", "set", "--cone", "--end-of-options", path],
                    opts,
                )
                .map_err(clone_failed)?;
        }
        let local = CallOptions {
            ceiling: Some(self.root),
            ..CallOptions::default()
        };
        self.git
            .output(
                temp,
                &["config", "--local", "--unset", "remote.origin.tagOpt"],
                local,
            )
            .map_err(|e| CloneOutcome::Failed {
                message: format!("can't unset remote.origin.tagOpt in the clone: {e}"),
            })?;
        Ok(())
    }

    /// Whether the user's config sets `core.sshCommand`, which batch mode's
    /// `GIT_SSH_COMMAND` would override: read from `dir`, the empty temp
    /// dir, where no repo's config applies (discovery stops at the root).
    fn configures_ssh(&self, dir: &Path) -> bool {
        let opts = CallOptions {
            ceiling: Some(self.root),
            ..CallOptions::default()
        };
        self.git
            .run(dir, &["config", "--get", "core.sshCommand"], opts)
            .is_ok_and(|out| out.status.success())
    }

    /// Reads the clone back at `target` as `repos status` would: present,
    /// on the recipe's branch (any, without one) tracking origin's branch
    /// of the name, clean.
    fn read_back(&self, entry: &Entry, recipe: &CloneRecipe, target: &Path) -> CloneOutcome {
        let fetches = RepoFetches::default();
        let run = probe(
            entry,
            ProbeContext {
                git: self.git,
                root: self.root,
                registry_dirs: self.registry_dirs,
                fetch: false,
                fetches: &fetches,
            },
        );
        let failed = |why: String| CloneOutcome::Failed {
            message: format!("cloned into {}, but {why}", target.display()),
        };
        let facts = match run.probed {
            Probed::Present(facts) => facts,
            Probed::Missing => return failed("nothing is there now".into()),
            Probed::NotARepo { detail } => return failed(format!("it's no repo: {detail}")),
            Probed::Failed { error, .. } => return failed(format!("reading it failed: {error}")),
        };
        let Head::Branch { name } = &facts.status.head else {
            return failed("its HEAD is detached".into());
        };
        if let Some(want) = &recipe.branch
            && name != want
        {
            return failed(format!("its HEAD is on {name}, not {want}"));
        }
        let Some(b) = facts.branches.iter().find(|b| b.branch.name == *name) else {
            return failed(format!("{name} has no commit"));
        };
        let upstream = format!("refs/remotes/origin/{name}");
        if b.branch.upstream_ref.as_deref() != Some(upstream.as_str()) {
            return failed(format!(
                "{name}'s upstream is {}, not {upstream}",
                b.branch.upstream_ref.as_deref().unwrap_or("none")
            ));
        }
        let uncommitted = facts.status.uncommitted.total();
        if uncommitted > 0 {
            return failed(format!(
                "its checkout has {uncommitted} uncommitted changes"
            ));
        }
        CloneOutcome::Cloned {
            branch: name.clone(),
            head: b.branch.oid.clone(),
        }
    }
}

/// What names a clone's temp dir, between the entry's dir and the pid.
const TEMP_DIR_INFIX: &str = ".repos-clone-";

/// The nonce's length in lowercase hex digits: a `u64`.
const NONCE_LEN: usize = 16;

/// This process's nonce, fixed at first use: random, from the OS-seeded
/// keys of std's `RandomState`, mixed with the time.
fn nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.finish()
    })
}

/// The name of the temp dir this process clones `dir` in:
/// `.<dir>.repos-clone-<pid>-<nonce>`, the same on every call.
pub fn temp_dir_name(dir: &str) -> String {
    format!(
        ".{dir}{TEMP_DIR_INFIX}{}-{:0NONCE_LEN$x}",
        std::process::id(),
        nonce()
    )
}

/// The dir the clone is made in, beside the entry's: hidden, and named for
/// the dir and this process (`temp_dir_name`).
fn temp_dir(root: &Path, dir: &str) -> PathBuf {
    root.join(temp_dir_name(dir))
}

/// Whether `name` is a clone's temp dir name (`temp_dir_name`): `.`, a dir
/// name, `.repos-clone-`, a pid's digits, `-`, and the nonce's
/// `NONCE_LEN` lowercase hex digits.
pub fn is_temp_dir_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((dir, tail)) = rest.rsplit_once(TEMP_DIR_INFIX) else {
        return false;
    };
    let Some((pid, nonce)) = tail.split_once('-') else {
        return false;
    };
    !dir.is_empty()
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && nonce.len() == NONCE_LEN
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A failed clone or sparse checkout, classified as a fetch failure is.
fn clone_failed(e: GitError) -> CloneOutcome {
    CloneOutcome::CloneFailed {
        failure: RemoteFailure::from_git_error(e, RefspecContext::default()),
    }
}

/// Moves the clone at `temp` to `target`: claims `target` by creating it
/// empty — failing when anything is there — then renames `temp` over it.
fn place(temp: &Path, target: &Path) -> Result<(), CloneOutcome> {
    match std::fs::create_dir(target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(CloneOutcome::Held {
                by: SyncHold::Changed,
            });
        }
        Err(e) => {
            return Err(CloneOutcome::Failed {
                message: format!("can't claim {}: {e}", target.display()),
            });
        }
    }
    std::fs::rename(temp, target).map_err(|e| {
        // the claim, if it's still the empty dir made above: `remove_dir`
        // removes nothing else
        let _ = std::fs::remove_dir(target);
        CloneOutcome::Failed {
            message: format!("can't move the clone into {}: {e}", target.display()),
        }
    })
}

/// Deletes the temp dir of a clone that failed or was held, returning
/// `outcome` — a failure noting the temp dir, should it survive.
fn discard(temp: &Path, outcome: CloneOutcome) -> CloneOutcome {
    let e = match std::fs::remove_dir_all(temp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => e,
        _ => return outcome,
    };
    let left = format!("{} is left behind ({e})", temp.display());
    match outcome {
        CloneOutcome::Failed { message } => CloneOutcome::Failed {
            message: format!("{message}; {left}"),
        },
        CloneOutcome::CloneFailed { failure } => CloneOutcome::Failed {
            message: format!("{}; {left}", failure_message(&failure)),
        },
        CloneOutcome::Held { .. } | CloneOutcome::Cloned { .. } => CloneOutcome::Failed {
            message: format!("not cloned; {left}"),
        },
    }
}

/// A classified failure in words, for a message that adds to it.
fn failure_message(f: &RemoteFailure) -> String {
    match f {
        RemoteFailure::RefGone { refname, .. } => format!("origin has no {refname}"),
        RemoteFailure::Unreachable { message, .. }
        | RemoteFailure::RepoNotFound { message }
        | RemoteFailure::Failed { message } => message.clone(),
        RemoteFailure::TimedOut { after_secs } => format!("timed out after {after_secs}s"),
        RemoteFailure::Rejected { reason, message } => message.as_ref().map_or_else(
            || format!("rejected ({reason})"),
            |message| format!("rejected ({reason}): {message}"),
        ),
        RemoteFailure::RefspecOutsideOrigin { refspec } => {
            format!("not run: refspec {refspec} writes outside refs/remotes/origin/")
        }
        RemoteFailure::OriginRefsShared { remote, refspec } => format!(
            "not run: remote {remote}'s refspec {refspec} can write under refs/remotes/origin/"
        ),
        RemoteFailure::LegacyRemotesUnreadable { path } => {
            format!("not run: the legacy remote {path} couldn't be read")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A temp dir holding a finished "clone" (a file in it) beside a target
    /// path, in a fresh dir.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let temp = tmp.path().join(".app.repos-clone-1-0123456789abcdef");
        std::fs::create_dir(&temp).unwrap();
        std::fs::write(temp.join("f"), "clone\n").unwrap();
        let target = tmp.path().join("app");
        (tmp, temp, target)
    }

    #[test]
    fn a_clone_moves_into_a_missing_path() {
        let (_tmp, temp, target) = fixture();
        assert_eq!(place(&temp, &target), Ok(()));
        assert_eq!(
            std::fs::read_to_string(target.join("f")).unwrap(),
            "clone\n"
        );
        assert!(!temp.exists());
    }

    /// The claim's the last check: whatever was made at the path in the
    /// instant before stays as it was, the clone left in its temp dir.
    #[test]
    fn a_clone_never_moves_over_anything() {
        type Make = fn(&Path);
        let made: [(&str, Make); 4] = [
            ("an empty dir", |p| std::fs::create_dir(p).unwrap()),
            ("a file", |p| std::fs::write(p, "mine\n").unwrap()),
            ("a dangling symlink", |p| {
                std::os::unix::fs::symlink("/nonexistent/x", p).unwrap();
            }),
            ("a symlink to a dir", |p| {
                let to = p.with_extension("real");
                std::fs::create_dir(&to).unwrap();
                std::os::unix::fs::symlink(&to, p).unwrap();
            }),
        ];
        for (what, make) in made {
            let (_tmp, temp, target) = fixture();
            make(&target);
            let before = std::fs::symlink_metadata(&target).unwrap();
            assert_eq!(
                place(&temp, &target),
                Err(CloneOutcome::Held {
                    by: SyncHold::Changed
                }),
                "{what}"
            );
            let after = std::fs::symlink_metadata(&target).unwrap();
            assert_eq!(
                (before.file_type(), before.len()),
                (after.file_type(), after.len()),
                "{what}"
            );
            assert!(temp.join("f").is_file(), "{what}");
        }
    }

    #[test]
    fn a_temp_dir_name_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let temp = temp_dir(tmp.path(), "app");
        let name = temp.file_name().unwrap().to_str().unwrap();
        assert!(is_temp_dir_name(name), "{name}");
        // fixed for the process
        assert_eq!(temp_dir_name("app"), name);
        assert_eq!(
            name,
            format!(".app.repos-clone-{}-{:016x}", std::process::id(), nonce())
        );
        for yes in [
            ".app.repos-clone-1-0123456789abcdef",
            ".tsv.fuz.dev.repos-clone-42-0000000000000000",
            "..x.repos-clone-7-ffffffffffffffff",
        ] {
            assert!(is_temp_dir_name(yes), "{yes}");
        }
        for no in [
            "app.repos-clone-1-0123456789abcdef",
            "..repos-clone-1-0123456789abcdef",
            ".app.repos-clone--0123456789abcdef",
            ".app.repos-clone-1x-0123456789abcdef",
            ".app.repos-clone-1-0123456789ABCDEF",
            ".app.repos-clone-1-0123456789abcde",
            ".app.repos-clone-1-0123456789abcdef0",
            ".app.repos-clone-1-0123456789abcdeg",
            ".app.repos-clone-1-0123456789abcdef-2",
            ".app.repos-clone-1-",
            ".app.repos-clone-1",
            ".app.repos-clone-",
            ".app.repos-clone",
            ".app",
            "",
        ] {
            assert!(!is_temp_dir_name(no), "{no}");
        }
    }

    /// Every failure reads as words, never as a debug dump.
    #[test]
    fn every_failure_has_its_own_words() {
        let words = [
            (
                RemoteFailure::RefGone {
                    refname: "refs/heads/x".into(),
                    fix: crate::remote::RefGoneFix::ByHand,
                },
                "origin has no refs/heads/x",
            ),
            (
                RemoteFailure::RepoNotFound {
                    message: "remote: Repository not found.".into(),
                },
                "remote: Repository not found.",
            ),
            (
                RemoteFailure::TimedOut { after_secs: 3 },
                "timed out after 3s",
            ),
            (
                RemoteFailure::Rejected {
                    reason: "pre-receive hook declined".into(),
                    message: None,
                },
                "rejected (pre-receive hook declined)",
            ),
            (
                RemoteFailure::RefspecOutsideOrigin {
                    refspec: "+refs/heads/*:refs/heads/*".into(),
                },
                "not run: refspec +refs/heads/*:refs/heads/* writes outside refs/remotes/origin/",
            ),
            (
                RemoteFailure::OriginRefsShared {
                    remote: "up".into(),
                    refspec: "+refs/heads/*:refs/remotes/origin/*".into(),
                },
                "not run: remote up's refspec +refs/heads/*:refs/remotes/origin/* can write \
                 under refs/remotes/origin/",
            ),
            (
                RemoteFailure::LegacyRemotesUnreadable {
                    path: ".git/remotes".into(),
                },
                "not run: the legacy remote .git/remotes couldn't be read",
            ),
        ];
        for (f, want) in words {
            assert_eq!(failure_message(&f), want);
        }
    }

    #[test]
    fn a_discarded_clone_is_gone() {
        let (_tmp, temp, _) = fixture();
        let outcome = CloneOutcome::CloneFailed {
            failure: RemoteFailure::TimedOut { after_secs: 1 },
        };
        assert_eq!(discard(&temp, outcome.clone()), outcome);
        assert!(!temp.exists());
        // gone already: nothing to say
        assert_eq!(discard(&temp, outcome.clone()), outcome);

        // one that can't be removed says where it is
        let (_tmp, temp, _) = fixture();
        let parent = temp.parent().unwrap().to_owned();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let kept = discard(&temp, outcome);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !temp.exists() {
            eprintln!("skipped: permissions don't bind this user (root)");
            return;
        }
        assert!(
            matches!(&kept, CloneOutcome::Failed { message }
                if message.starts_with("timed out after 1s; ")
                    && message.contains(".app.repos-clone-1-0123456789abcdef is left behind")),
            "{kept:?}"
        );
    }
}
