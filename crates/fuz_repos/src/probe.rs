//! The per-entry probe: the git calls and file reads behind `classify`.
//!
//! Nothing here writes a working tree, an index, or a
//! local branch; only the optional fetch touches the network, and it writes
//! remote-tracking refs alone.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::git::{CallOptions, Git, GitError, NetworkOptions};
use crate::porcelain::{self, ConfigFacts, RefFacts, StatusFacts, Track};
use crate::registry::{CheckoutMode, Entry};
use crate::state::{InProgressOp, Layout};

/// What the probe needs from its caller.
#[derive(Debug, Clone, Copy)]
pub struct ProbeContext<'a> {
    pub git: &'a Git,
    pub root: &'a Path,
    pub now: SystemTime,
    /// Fetch owned, non-pinned entries from `origin` before probing.
    pub fetch: bool,
}

/// One entry's probe, with its timings.
#[derive(Debug)]
pub struct ProbeRun {
    pub probed: Probed,
    /// `None` when no fetch was attempted; `Some(Err)` carries git's message.
    pub fetch: Option<Result<(), String>>,
    pub fetch_time: Duration,
    pub probe_time: Duration,
}

/// What the probe found.
#[derive(Debug)]
pub enum Probed {
    Missing,
    /// The dir exists but holds no repo; `detail` says why.
    NotARepo {
        detail: String,
    },
    Present(Box<RepoFacts>),
    /// The dir is a repo, but a later call failed.
    Failed {
        error: String,
    },
}

/// The facts of a present repo.
#[derive(Debug, Clone)]
pub struct RepoFacts {
    /// The primary checkout's path.
    pub path: String,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub config: ConfigFacts,
    pub status: StatusFacts,
    pub in_progress: Option<InProgressOp>,
    pub branches: Vec<BranchFacts>,
    pub layout: Layout,
    /// `FETCH_HEAD`'s age; `None` when the repo was never fetched.
    pub fetched_age_secs: Option<u64>,
}

/// A local branch's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchFacts {
    pub branch: RefFacts,
    /// Commits on no remote-tracking ref, minus shallow roots; counted only
    /// where `could_carry_local_work` says so, else zero.
    pub unique_commits: u32,
    /// In a shallow clone, whether the branch's unique commits sit on the
    /// fetched tip (its upstream is an ancestor); false elsewhere.
    pub on_fetched_tip: bool,
}

/// Whether a branch might hold commits on no remote: anything but a branch
/// level with, or strictly behind, a resolved upstream.
pub const fn could_carry_local_work(r: &RefFacts) -> bool {
    !matches!(
        (&r.upstream_ref, r.track),
        (Some(_), Track::Even | Track::Behind(_))
    )
}

/// Whether `--fetch` fetches this entry: owned and not pinned.
pub fn fetches(entry: &Entry) -> bool {
    entry.writable && entry.checkout_mode != CheckoutMode::Pinned
}

/// Probes one entry.
pub fn probe(entry: &Entry, cx: ProbeContext<'_>) -> ProbeRun {
    let start = Instant::now();
    let mut fetch = FetchRun::default();
    let probed = probe_present(entry, &cx.root.join(&entry.dir), cx, &mut fetch)
        .unwrap_or_else(|error| Probed::Failed { error });
    ProbeRun {
        probed,
        fetch: fetch.result,
        fetch_time: fetch.time,
        probe_time: start.elapsed().saturating_sub(fetch.time),
    }
}

/// The fetch step's outcome, recorded even when a later step fails.
#[derive(Debug, Default)]
struct FetchRun {
    result: Option<Result<(), String>>,
    time: Duration,
}

fn probe_present(
    entry: &Entry,
    dir: &Path,
    cx: ProbeContext<'_>,
    fetch: &mut FetchRun,
) -> Result<Probed, String> {
    // 1. presence
    if !dir.exists() {
        return Ok(Probed::Missing);
    }
    let path = dir
        .to_str()
        .ok_or_else(|| format!("non-UTF-8 path {}", dir.display()))?
        .to_owned();
    let local = CallOptions {
        ceiling: Some(cx.root),
        network: None,
    };
    let dirs = match cx.git.output_string(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
        local,
    ) {
        Ok(out) => out,
        Err(GitError::Failed { stderr, .. }) => {
            return Ok(Probed::NotARepo {
                detail: not_a_repo_detail(dir, &stderr),
            });
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut lines = dirs.lines();
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err(format!("rev-parse: unexpected output `{dirs}`"));
    };
    let git_dir = PathBuf::from(git_dir);
    let common_dir = PathBuf::from(common_dir);

    // 4. config, first: fetch needs to know whether SSH is configured
    let config = match cx.git.run(
        dir,
        &["config", "-z", "--get-regexp", porcelain::CONFIG_PATTERN],
        local,
    ) {
        // exit 1 is "no matching keys"
        Ok(out) if out.status.success() || out.status.code() == Some(1) => {
            ConfigFacts::parse(&out.stdout)?
        }
        Ok(out) => return Err(format!("config failed: {}", out.stderr.trim())),
        Err(e) => return Err(e.to_string()),
    };
    let shallow_roots = read_shallow_roots(&common_dir);

    if cx.fetch && fetches(entry) {
        let start = Instant::now();
        let mut args = vec!["fetch", "--prune", "--quiet"];
        if !shallow_roots.is_empty() {
            args.extend(["--depth", "1"]);
        }
        args.push("origin");
        let net = CallOptions {
            ceiling: Some(cx.root),
            network: Some(NetworkOptions {
                batch_ssh: !config.ssh_command && !Git::env_configures_ssh(),
            }),
        };
        fetch.result = Some(
            cx.git
                .output(dir, &args, net)
                .map(drop)
                .map_err(|e| match e {
                    GitError::Failed { stderr, .. } => stderr,
                    e => e.to_string(),
                }),
        );
        fetch.time = start.elapsed();
    }
    // a fetch may have added shallow roots
    let shallow_roots = if fetch.result.is_some() {
        read_shallow_roots(&common_dir)
    } else {
        shallow_roots
    };

    // 2. status of the primary checkout
    let status = cx
        .git
        .output(
            dir,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--show-stash",
                "--no-ahead-behind",
                "--no-renames",
                "--untracked-files=normal",
                "-z",
            ],
            local,
        )
        .map_err(|e| e.to_string())?;
    let status = porcelain::parse_status(&status)?;

    // 3. branches
    let format = format!("--format={}", porcelain::REFS_FORMAT);
    let refs = cx
        .git
        .output(dir, &["for-each-ref", &format, "refs/heads"], local)
        .map_err(|e| e.to_string())?;
    let refs = porcelain::parse_refs(&refs)?;

    // 5. unique commits, where local work could be
    let mut branches = Vec::with_capacity(refs.len());
    for r in refs {
        let (unique_commits, on_fetched_tip) = if could_carry_local_work(&r) {
            count_unique(cx.git, dir, &r, &shallow_roots, local)?
        } else {
            (0, false)
        };
        branches.push(BranchFacts {
            branch: r,
            unique_commits,
            on_fetched_tip,
        });
    }

    // 7. files
    let in_progress = read_in_progress(&git_dir);
    let fetched_age_secs = std::fs::metadata(git_dir.join("FETCH_HEAD"))
        .and_then(|m| m.modified())
        .ok()
        .map(|t| cx.now.duration_since(t).unwrap_or_default().as_secs());
    let layout = Layout {
        shallow: !shallow_roots.is_empty(),
        sparse: config.sparse,
        partial_filter: config.partial_filter.clone(),
    };

    Ok(Probed::Present(Box::new(RepoFacts {
        path,
        git_dir,
        common_dir,
        config,
        status,
        in_progress,
        branches,
        layout,
        fetched_age_secs,
    })))
}

/// Why a dir isn't a repo: empty (a clone that never started), files with no
/// `.git` (a copy or an unpacked archive, not a clone), or git's message when
/// a `.git` is there but unusable (corrupt, dubious ownership, …).
fn not_a_repo_detail(dir: &Path, stderr: &str) -> String {
    if std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none()) {
        return "empty directory".into();
    }
    if dir.is_dir() && !dir.join(".git").exists() {
        return "no .git: a copy of the files, not a clone".into();
    }
    let line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(stderr)
        .trim();
    line.strip_prefix("fatal: ").unwrap_or(line).to_owned()
}

/// Counts a branch's commits on no remote. In a shallow clone every fetched
/// commit is a root (the clone recipe is depth 1), so roots are subtracted —
/// a tip fetched rather than made here isn't local work — and for what
/// remains, whether it sits on the fetched tip.
fn count_unique(
    git: &Git,
    dir: &Path,
    r: &RefFacts,
    shallow_roots: &HashSet<String>,
    opts: CallOptions<'_>,
) -> Result<(u32, bool), String> {
    let rev = format!("refs/heads/{}", r.name);
    if shallow_roots.is_empty() {
        let n = git
            .output_string(
                dir,
                &["rev-list", "--count", &rev, "--not", "--remotes"],
                opts,
            )
            .map_err(|e| e.to_string())?;
        let n = n
            .trim()
            .parse()
            .map_err(|_| format!("rev-list --count: `{}`", n.trim()))?;
        return Ok((n, false));
    }
    let out = git
        .output_string(dir, &["rev-list", &rev, "--not", "--remotes"], opts)
        .map_err(|e| e.to_string())?;
    let n = out.lines().filter(|c| !shallow_roots.contains(*c)).count();
    let n = u32::try_from(n).unwrap_or(u32::MAX);
    let on_fetched_tip = match (&r.upstream_ref, n) {
        (Some(upstream), 1..) => {
            let out = git
                .run(dir, &["merge-base", "--is-ancestor", upstream, &rev], opts)
                .map_err(|e| e.to_string())?;
            match out.status.code() {
                Some(0) => true,
                Some(1) => false,
                _ => {
                    return Err(format!(
                        "merge-base --is-ancestor failed: {}",
                        out.stderr.trim()
                    ));
                }
            }
        }
        _ => false,
    };
    Ok((n, on_fetched_tip))
}

/// The commits in `<commondir>/shallow`; empty for a full clone.
fn read_shallow_roots(common_dir: &Path) -> HashSet<String> {
    std::fs::read_to_string(common_dir.join("shallow"))
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// An operation stopped mid-way, from its marker in the checkout's git dir.
/// Never `REBASE_HEAD`: git leaves it behind after a finished rebase.
fn read_in_progress(git_dir: &Path) -> Option<InProgressOp> {
    [
        ("rebase-merge", InProgressOp::Rebase),
        ("rebase-apply", InProgressOp::Rebase),
        ("MERGE_HEAD", InProgressOp::Merge),
        ("CHERRY_PICK_HEAD", InProgressOp::CherryPick),
        ("REVERT_HEAD", InProgressOp::Revert),
        ("BISECT_LOG", InProgressOp::Bisect),
        ("sequencer", InProgressOp::Sequencer),
    ]
    .into_iter()
    .find(|(marker, _)| git_dir.join(marker).exists())
    .map(|(_, op)| op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    fn r(upstream: Option<&str>, track: Track) -> RefFacts {
        RefFacts {
            name: "b".into(),
            upstream_ref: upstream.map(str::to_owned),
            track,
            worktree: None,
            committer_time: 0,
        }
    }

    #[test]
    fn local_work_is_possible_off_an_even_or_behind_upstream() {
        let up = Some("refs/remotes/origin/b");
        assert!(!could_carry_local_work(&r(up, Track::Even)));
        assert!(!could_carry_local_work(&r(up, Track::Behind(3))));
        assert!(could_carry_local_work(&r(up, Track::Ahead(1))));
        assert!(could_carry_local_work(&r(
            up,
            Track::Diverged {
                ahead: 1,
                behind: 1
            }
        )));
        assert!(could_carry_local_work(&r(up, Track::Gone)));
        assert!(could_carry_local_work(&r(None, Track::Even)));
    }

    #[test]
    fn in_progress_ignores_a_stale_rebase_head() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("REBASE_HEAD"), "abc\n").unwrap();
        assert_eq!(read_in_progress(tmp.path()), None);
        std::fs::create_dir(tmp.path().join("rebase-merge")).unwrap();
        assert_eq!(read_in_progress(tmp.path()), Some(InProgressOp::Rebase));
    }

    #[test]
    fn not_a_repo_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Registry::parse(
            r#"
owners = ["me"]
[repos.empty]
url = "https://github.com/me/empty"
visibility = "public"
purpose = "a clone that never started"
[repos.plain]
url = "https://github.com/me/plain"
visibility = "public"
purpose = "a dir that isn't a checkout"
[repos.stub]
url = "https://github.com/me/stub"
visibility = "public"
purpose = "a .git git can't use"
"#,
        )
        .unwrap();
        std::fs::create_dir(tmp.path().join("empty")).unwrap();
        std::fs::create_dir(tmp.path().join("plain")).unwrap();
        std::fs::write(tmp.path().join("plain/file"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join("stub/.git")).unwrap();
        let git = Git::new();
        let cx = ProbeContext {
            git: &git,
            root: tmp.path(),
            now: SystemTime::now(),
            fetch: false,
        };
        let details: Vec<String> = registry
            .entries()
            .iter()
            .map(|e| match probe(e, cx).probed {
                Probed::NotARepo { detail } => detail,
                p => panic!("{}: {p:?}", e.key),
            })
            .collect();
        assert_eq!(details[0], "empty directory");
        assert_eq!(details[1], "no .git: a copy of the files, not a clone");
        assert!(
            details[2].contains("not a git repository"),
            "{}",
            details[2]
        );
    }
}
