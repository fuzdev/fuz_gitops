//! The `--json` contract's golden fixtures: hand-built documents, serialized
//! and compared with the JSON checked in at the repo root's
//! `src/test/fixtures/repos_status/`, where the TS side reads them.
//!
//! Compared as parsed JSON, so a formatter's reflow of a checked-in file
//! can't fail the test. Never hand-edit the files: regenerate them with
//! `UPDATE_GOLDEN=1 cargo test --test golden`, and bump
//! `STATUS_FORMAT_VERSION` when the change breaks the shape.
//!
//! Between them the documents cover every variant of the report's enums,
//! each domain's built in its own function so that an every-variant coverage
//! floor (an exhaustive `match` per enum that fails to compile when a
//! variant lands uncovered) can attach beside it to keep it so.

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use fuz_repos::STATUS_FORMAT_VERSION;
use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::error::Error;
use fuz_repos::registry::{
    CheckoutList, CheckoutMode, EntryKind, EntryName, RegistryIssue, Visibility,
};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    EntryStatus, ErrorReport, RepairBlock, StatusReport, UnregisteredClone, UnregisteredKind,
};
use fuz_repos::state::{
    BranchNeedsHuman, BranchStatus, Checkout, CleanupReason, GitDirHolds, Head, HeldBy,
    InProgressOp, Layout, Presence, Prune, PruneLoss, Relation, SyncAction, Uncommitted,
    UnprobedHead, UnprobedWhy, UnprobedWorktree, UnprobedWorktreeStatus, Verdict,
};
use serde::Serialize;
use serde_json::Value;

/// The fixed clock every timestamp is built from.
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

const WORKSPACE: &str = "/home/me/dev";

/// `src/test/fixtures/repos_status/` under the repo root, two above the
/// crate.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("src/test/fixtures/repos_status")
}

/// Compares `doc` with the fixture `name`, or rewrites the fixture under
/// `UPDATE_GOLDEN` (tab-indented, as the repo's other JSON is).
fn assert_golden(name: &str, doc: &impl Serialize) {
    let path = fixtures_dir().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| !v.is_empty()) {
        let mut buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        doc.serialize(&mut ser).unwrap();
        buf.push(b'\n');
        std::fs::create_dir_all(fixtures_dir()).unwrap();
        std::fs::write(&path, buf).unwrap();
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — generate it with `UPDATE_GOLDEN=1 cargo test --test golden`",
            path.display()
        )
    });
    let golden: Value = serde_json::from_str(&text).unwrap();
    let actual = serde_json::to_value(doc).unwrap();
    if let Some(at) = first_difference(&golden, &actual, String::new()) {
        panic!(
            "{} drifted from the serialized document at `{at}`: if the change is intended, \
             regenerate with `UPDATE_GOLDEN=1 cargo test --test golden`, and bump STATUS_FORMAT_VERSION if it breaks the shape",
            path.display()
        );
    }
}

/// The JSON pointer of the first place `a` and `b` differ.
fn first_difference(a: &Value, b: &Value, at: String) -> Option<String> {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter().find_map(|k| match (a.get(k), b.get(k)) {
                (Some(x), Some(y)) => first_difference(x, y, format!("{at}/{k}")),
                _ => Some(format!("{at}/{k}")),
            })
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(i, (x, y))| first_difference(x, y, format!("{at}/{i}"))),
        _ => (a != b).then_some(at),
    }
}

#[test]
fn status_report() {
    let doc = status_report_doc();
    assert_eq!(doc.version, STATUS_FORMAT_VERSION);
    assert!(doc.fetched);
    assert_golden("status_report.json", &doc);
}

#[test]
fn status_report_targeted() {
    let doc = targeted_doc();
    // targets narrowed the run: no scan, `null` — never `[]`, which reads as
    // "none found"
    assert!(doc.unregistered.is_none());
    // no `--fetch`: no entry was fetched or checked
    assert!(!doc.fetched);
    assert!(
        doc.entries
            .iter()
            .all(|e| e.fetch_error.is_none() && e.visibility_check.is_none())
    );
    assert_golden("status_report_targeted.json", &doc);
}

#[test]
fn error_report() {
    let doc = error_doc();
    assert_eq!(doc.version, STATUS_FORMAT_VERSION);
    assert_golden("error_report.json", &doc);
}

#[test]
fn a_drifted_golden_names_where() {
    let a = serde_json::json!({"entries": [{"key": "app", "stashes": 0}], "version": 3});
    let b = serde_json::json!({"entries": [{"key": "app", "stashes": 1}], "version": 3});
    assert_eq!(
        first_difference(&a, &b, String::new()).as_deref(),
        Some("/entries/0/stashes")
    );
    assert_eq!(first_difference(&a, &a, String::new()), None);
    let c = serde_json::json!({"entries": [], "version": 3});
    assert_eq!(
        first_difference(&a, &c, String::new()).as_deref(),
        Some("/entries")
    );
    let d = serde_json::json!({"entries": [{"key": "app", "stashes": 0}]});
    assert_eq!(
        first_difference(&a, &d, String::new()).as_deref(),
        Some("/version")
    );
}

// --- the documents ---

/// A whole-workspace run under `--fetch`: every entry shape, every fetch
/// failure and visibility check, and the unregistered scan.
fn status_report_doc() -> StatusReport {
    let mut report = StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        true,
        vec![
            app(),
            blog(),
            archived(),
            test262(),
            spec(),
            zzz(),
            missing(),
            not_a_repo(),
            partial(),
            forge(),
            zap(),
            site(),
            mdz(),
            tsv(),
            tsv_fuz_dev(),
            fuz_css(),
            fuz_ui(),
            uz(),
        ],
    );
    report.unregistered = Some(unregistered());
    report
}

/// A run narrowed by targets, local refs only: the scan, the fetch, and
/// the visibility check didn't run.
fn targeted_doc() -> StatusReport {
    StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        false,
        vec![EntryStatus {
            needs_human: vec![NeedsHuman::OriginMismatch {
                origin: OriginRemote::Missing,
                expected: "git@github.com:me/gro".into(),
                fix: OriginFix::Add,
            }],
            ..entry("gro", follow("main"))
        }],
    )
}

/// A fatal error under `--json`, the kind with the richest payload.
fn error_doc() -> ErrorReport {
    ErrorReport::new(&Error::RegistryInvalid {
        path: PathBuf::from(format!("{WORKSPACE}/repos.toml")),
        issues: registry_issues(),
    })
}

// --- entries ---

fn path(dir: &str) -> String {
    format!("{WORKSPACE}/{dir}")
}

fn follow(branch: &str) -> CheckoutMode {
    CheckoutMode::Follow {
        branch: branch.into(),
    }
}

const fn plain_layout() -> Layout {
    Layout {
        shallow: false,
        sparse: false,
        partial_filter: None,
    }
}

fn primary(dir: &str, head: Head) -> Checkout {
    Checkout {
        path: path(dir),
        primary: true,
        head,
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: false,
        submodules: None,
    }
}

fn on(name: &str) -> Head {
    Head::Branch { name: name.into() }
}

/// A present, owned, public repo on `main`, fetched a day ago, with nothing
/// to say.
fn entry(key: &str, mode: CheckoutMode) -> EntryStatus {
    EntryStatus {
        key: key.into(),
        kind: EntryKind::Repo,
        dir: key.into(),
        url: format!("https://github.com/me/{key}"),
        writable: true,
        archived: false,
        visibility: Some(Visibility::Public),
        ci: true,
        checkout_mode: mode,
        presence: Presence::Present,
        layout: Some(plain_layout()),
        checkouts: vec![primary(key, on("main"))],
        branches: vec![],
        stashes: 0,
        fetched_at: Some(NOW - DAY),
        needs_human: vec![],
        probe_error: None,
        unprobed_worktrees: vec![],
        fetch_error: None,
        visibility_check: None,
    }
}

fn branch(
    name: &str,
    upstream: Option<&str>,
    relation: Relation,
    verdict: Verdict,
) -> BranchStatus {
    BranchStatus {
        name: name.into(),
        upstream: upstream.map(str::to_owned),
        worktree: None,
        unique_commits: 0,
        newest_commit_at: NOW - 2 * DAY,
        relation,
        verdict,
    }
}

/// The busy repo: every relation but `Shallow` (`test262()`), every verdict
/// kind, every `HeldBy` but `Entry` (`blog()`), linked worktrees, and every
/// way a worktree goes unprobed.
fn app() -> EntryStatus {
    let mut e = entry("app", follow("main"));
    e.fetch_error = Some(RemoteFailure::Failed {
        message: "fatal: protocol error: bad line length character: Welc".into(),
    });
    e.stashes = 2;
    e.checkouts[0].uncommitted = Uncommitted {
        staged: 1,
        unstaged: 2,
        untracked: 3,
        conflicted: 0,
    };
    e.checkouts.push(Checkout {
        path: path("app-feat"),
        primary: false,
        head: on("feat"),
        uncommitted: Uncommitted {
            staged: 0,
            unstaged: 1,
            untracked: 0,
            conflicted: 0,
        },
        in_progress: None,
        locked: false,
        linked: true,
        submodules: None,
    });
    e.checkouts.push(Checkout {
        path: "/home/me/wt/app-old".into(),
        primary: false,
        head: on("old"),
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: Some(false),
    });
    e.checkouts.push(Checkout {
        path: path("app-fix"),
        primary: false,
        head: Head::Detached {
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
        },
        uncommitted: Uncommitted {
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 1,
        },
        in_progress: Some(InProgressOp::CherryPick),
        locked: true,
        linked: true,
        submodules: Some(true),
    });
    e.branches = vec![
        BranchStatus {
            unique_commits: 2,
            worktree: Some(path("app")),
            ..branch(
                "main",
                Some("origin/main"),
                Relation::Ahead { commits: 2 },
                Verdict::Act {
                    action: SyncAction::Push { commits: 2 },
                },
            )
        },
        BranchStatus {
            worktree: Some(path("app-feat")),
            ..branch(
                "feat",
                Some("origin/feat"),
                Relation::Behind { commits: 1 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: HeldBy::DirtyCheckout,
                },
            )
        },
        branch(
            "usb",
            Some("origin/usb"),
            Relation::Behind { commits: 4 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 4 },
                by: HeldBy::UnprobedWorktree,
            },
        ),
        BranchStatus {
            unique_commits: 2,
            ..branch(
                "arc",
                Some("origin/arc"),
                Relation::Diverged {
                    ahead: 2,
                    behind: 5,
                },
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            )
        },
        BranchStatus {
            unique_commits: 3,
            ..branch(
                "fork",
                Some("origin/fork"),
                Relation::Unmapped,
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Unmapped,
                },
            )
        },
        BranchStatus {
            unique_commits: 1,
            newest_commit_at: NOW - 40 * DAY,
            ..branch("wip", None, Relation::Untracked, Verdict::LocalOnly)
        },
        BranchStatus {
            worktree: Some("/home/me/wt/app-old".into()),
            ..branch(
                "old",
                Some("origin/old"),
                Relation::Gone,
                Verdict::Cleanup {
                    reason: CleanupReason::UpstreamGone,
                    removable_worktree: Some("/home/me/wt/app-old".into()),
                },
            )
        },
        branch(
            "done",
            None,
            Relation::Untracked,
            Verdict::Cleanup {
                reason: CleanupReason::Merged,
                removable_worktree: None,
            },
        ),
        branch(
            "synced",
            Some("origin/synced"),
            Relation::InSync,
            Verdict::Quiet,
        ),
        branch(
            "theirs",
            Some("upstream/main"),
            Relation::Untracked,
            Verdict::Quiet,
        ),
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("app-fix"),
            op: InProgressOp::CherryPick,
        },
        NeedsHuman::OperationInProgress {
            checkout: "/media/usb/app".into(),
            op: InProgressOp::Bisect,
        },
        NeedsHuman::WorktreeUnreadable {
            path: path("app/.git/worktrees/x"),
        },
    ];
    e.unprobed_worktrees = unprobed_worktrees();
    e
}

fn git_dir(id: &str) -> String {
    path(&format!("app/.git/worktrees/{id}"))
}

fn unprobed(dir: &str, head: UnprobedHead, why: UnprobedWhy) -> UnprobedWorktree {
    UnprobedWorktree {
        path: path(dir),
        git_dir: Some(git_dir(dir)),
        head,
        locked: false,
        in_progress: None,
        why,
        holds: None,
    }
}

fn unprobed_on(name: &str) -> UnprobedHead {
    UnprobedHead::Branch { name: name.into() }
}

/// Every `UnprobedWhy`, every `UnprobedHead`, every `Prune`, and every
/// `PruneLoss`.
fn unprobed_worktrees() -> Vec<UnprobedWorktreeStatus> {
    let nothing_held = GitDirHolds {
        submodules: false,
        worktree_refs: false,
        staged: Some(false),
    };
    vec![
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(nothing_held),
                ..unprobed("app-gone", unprobed_on("gone"), UnprobedWhy::Prunable)
            },
            prune: Some(Prune::Safe),
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                in_progress: Some(InProgressOp::Revert),
                holds: Some(GitDirHolds {
                    submodules: true,
                    worktree_refs: true,
                    staged: None,
                }),
                ..unprobed(
                    "app-spike",
                    UnprobedHead::Detached {
                        commit: "89abcdef0123456789abcdef0123456789abcdef".into(),
                    },
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: prune_losses(),
            }),
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(nothing_held),
                ..unprobed("app-b", unprobed_on("b"), UnprobedWhy::Prunable)
            },
            prune: Some(Prune::Moved {
                to: vec!["b-copy".into(), "b-moved".into()],
            }),
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                path: "/media/usb/app".into(),
                locked: true,
                in_progress: Some(InProgressOp::Bisect),
                ..unprobed("usb", unprobed_on("usb"), UnprobedWhy::Missing)
            },
            prune: None,
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                path: git_dir("x"),
                git_dir: None,
                ..unprobed(
                    "x",
                    UnprobedHead::Unknown,
                    UnprobedWhy::Failed {
                        error: "not listed by git: reading HEAD: Permission denied".into(),
                    },
                )
            },
            prune: None,
        },
    ]
}

fn prune_losses() -> Vec<PruneLoss> {
    vec![
        PruneLoss::Operation {
            op: InProgressOp::Revert,
        },
        PruneLoss::DetachedHead,
        PruneLoss::UnknownHead,
        PruneLoss::MissingBranch {
            name: "spike".into(),
        },
        PruneLoss::Submodules,
        PruneLoss::WorktreeRefs,
        PruneLoss::StagedChanges,
        PruneLoss::UnmatchedGitDir,
        PruneLoss::RelativeGitdir {
            git_dir: git_dir("k"),
        },
    ]
}

/// Origin drift, holding the entry's push; a merge in its primary; the
/// fetch failed.
fn blog() -> EntryStatus {
    let mut e = entry("fuz_blog", follow("main"));
    e.url = "https://github.com/fuzdev/fuz_blog".into();
    e.checkouts[0].in_progress = Some(InProgressOp::Merge);
    e.branches = vec![BranchStatus {
        unique_commits: 2,
        worktree: Some(path("fuz_blog")),
        ..branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 2 },
            Verdict::Held {
                action: SyncAction::Push { commits: 2 },
                by: HeldBy::Entry,
            },
        )
    }];
    e.needs_human = vec![
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:ryanatkn/fuz_blog".into(),
            },
            expected: "git@github.com:fuzdev/fuz_blog".into(),
            fix: OriginFix::SetUrl,
        },
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_blog"),
            op: InProgressOp::Merge,
        },
    ];
    e.fetch_error = Some(RemoteFailure::RefGone {
        refname: "refs/heads/dev".into(),
        fix: RefGoneFix::SetBranches {
            branch: Some("main".into()),
        },
    });
    e
}

/// Archived and private, no CI, with a commit ahead its host refuses, and
/// no `origin` — and anyone can read it.
fn archived() -> EntryStatus {
    let mut e = entry("old", follow("main"));
    e.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Missing,
        expected: "git@github.com:me/old".into(),
        fix: OriginFix::Add,
    }];
    e.archived = true;
    e.visibility = Some(Visibility::Private);
    e.visibility_check = Some(VisibilityCheck::Leak);
    e.ci = false;
    e.fetched_at = Some(NOW - 300 * DAY);
    e.branches = vec![BranchStatus {
        unique_commits: 1,
        worktree: Some(path("old")),
        ..branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 1 },
            Verdict::NeedsHuman {
                reason: BranchNeedsHuman::ArchivedAhead,
            },
        )
    }];
    e
}

/// A third-party reference left at its HEAD: shallow and sparse, detached
/// mid-rebase, one branch to move and one with local work off the tip.
fn test262() -> EntryStatus {
    let mut e = entry("test262", CheckoutMode::Head);
    e.kind = EntryKind::Reference;
    e.url = "https://github.com/tc39/test262".into();
    e.writable = false;
    e.visibility = None;
    e.ci = false;
    e.layout = Some(Layout {
        shallow: true,
        sparse: true,
        partial_filter: None,
    });
    e.checkouts[0].head = Head::Detached {
        commit: "fedcba9876543210fedcba9876543210fedcba98".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Rebase);
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Shallow,
            Verdict::Act {
                action: SyncAction::Move,
            },
        ),
        BranchStatus {
            unique_commits: 2,
            ..branch(
                "work",
                Some("origin/work"),
                Relation::Shallow,
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::ShallowLocalWork,
                },
            )
        },
        BranchStatus {
            unique_commits: 4,
            ..branch("audit", None, Relation::Untracked, Verdict::LocalOnly)
        },
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("test262"),
            op: InProgressOp::Rebase,
        },
        // its URL list reset by an empty value
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "https://github.com/tc39/test262".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::EmptyValue,
            },
        },
    ];
    e.fetched_at = None;
    e
}

/// An owned fork kept as a reference, pinned but found on a branch, with a
/// `git am` stopped mid-way and a valueless `origin` URL.
fn spec() -> EntryStatus {
    let mut e = entry("ecma262", CheckoutMode::Pinned);
    e.kind = EntryKind::Reference;
    e.visibility = None;
    e.checkouts[0].head = on("draft");
    e.checkouts[0].in_progress = Some(InProgressOp::Am);
    e.needs_human = vec![
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/ecma262".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::ValuelessUrl,
            },
        },
        NeedsHuman::PinnedOnBranch {
            branch: "draft".into(),
        },
        NeedsHuman::OperationInProgress {
            checkout: path("ecma262"),
            op: InProgressOp::Am,
        },
    ];
    e
}

/// Following `dev`, which is missing, detached where it shouldn't be, with a
/// sequencer stopped in it; `main` has no upstream.
fn zzz() -> EntryStatus {
    let mut e = entry("zzz", follow("dev"));
    e.fetch_error = Some(RemoteFailure::Unreachable {
        cause: UnreachableCause::Connection,
        message: "ssh: connect to host github.com port 22: Connection refused".into(),
    });
    e.checkouts[0].head = Head::Detached {
        commit: "00112233445566778899aabbccddeeff00112233".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Sequencer);
    e.branches = vec![branch("main", None, Relation::Untracked, Verdict::Quiet)];
    e.needs_human = vec![
        NeedsHuman::DefaultBranchMissing {
            branch: "dev".into(),
        },
        NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into(),
        },
        NeedsHuman::UnexpectedDetached {
            checkout: path("zzz"),
        },
        NeedsHuman::OperationInProgress {
            checkout: path("zzz"),
            op: InProgressOp::Sequencer,
        },
    ];
    e
}

/// Not cloned yet: sync would clone it.
fn missing() -> EntryStatus {
    EntryStatus {
        presence: Presence::Missing,
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        ..entry("blake3", follow("main"))
    }
}

/// A dir that holds no repo.
fn not_a_repo() -> EntryStatus {
    EntryStatus {
        presence: Presence::NotARepo,
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        needs_human: vec![NeedsHuman::NotARepo {
            detail: "empty directory".into(),
        }],
        ..entry("goblins", follow("main"))
    }
}

/// A partial clone whose probe failed on a missing object: the layout read
/// first stands, the rest is incomplete.
fn partial() -> EntryStatus {
    EntryStatus {
        layout: Some(Layout {
            partial_filter: Some("tree:0".into()),
            ..plain_layout()
        }),
        checkouts: vec![],
        fetched_at: None,
        probe_error: Some("git status failed (128): error: bad tree object HEAD".into()),
        fetch_error: Some(RemoteFailure::RepoNotFound {
            message: "ERROR: Repository not found.".into(),
        }),
        ..entry("wpt", follow("main"))
    }
}

/// Private as declared; its key refused.
fn forge() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Auth,
            message: "git@github.com: Permission denied (publickey).".into(),
        }),
        visibility_check: Some(VisibilityCheck::Private),
        ..entry("fuz_forge", follow("main"))
    }
}

/// Private, with the network down: its host not found, the check timed out.
fn zap() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Dns,
            message: "ssh: Could not resolve hostname github.com: Temporary failure in name \
                      resolution"
                .into(),
        }),
        visibility_check: Some(VisibilityCheck::Unknown {
            failure: RemoteFailure::TimedOut { after_secs: 120 },
        }),
        ..entry("zap", follow("main"))
    }
}

/// A branch deleted on the remote, named by one of several fetch refspecs;
/// an old `origin` URL beside a mirror.
fn mdz() -> EntryStatus {
    EntryStatus {
        needs_human: vec![NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:old/mdz".into(),
            },
            expected: "git@github.com:me/mdz".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::SeveralUrls,
            },
        }],
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/attrs".into(),
            fix: RefGoneFix::UnsetRefspec {
                pattern: r"^\+?refs/heads/attrs(:|$)".into(),
            },
        }),
        ..entry("mdz", follow("main"))
    }
}

/// A single-branch clone whose own branch, the registry's, is gone: no
/// branch to name.
fn tsv() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/main".into(),
            fix: RefGoneFix::SetBranches { branch: None },
        }),
        ..entry("tsv", follow("main"))
    }
}

/// A remote named `origin/fork`, its refs under origin's: `status --fetch`
/// doesn't fetch.
fn fuz_css() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::OriginRefsShared {
            remote: "origin/fork".into(),
            refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
        }),
        ..entry("fuz_css", follow("main"))
    }
}

/// A legacy remotes file that can't be read: `status --fetch` doesn't
/// fetch.
fn fuz_ui() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::LegacyRemotesUnreadable {
            path: path("fuz_ui/.git/remotes/old"),
        }),
        ..entry("fuz_ui", follow("main"))
    }
}

/// A refspec writing tags: `status --fetch` doesn't fetch it.
fn tsv_fuz_dev() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefspecOutsideOrigin {
            refspec: "+refs/tags/*:refs/tags/*".into(),
        }),
        ..entry("tsv.fuz.dev", follow("main"))
    }
}

/// A missing ref no refspec in the repo's config names.
fn uz() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "typecheck-arc".into(),
            fix: RefGoneFix::ByHand,
        }),
        ..entry("uz", follow("main"))
    }
}

/// A host key ssh doesn't trust; an old origin, with a token in it
/// (redacted), set in global config.
fn site() -> EntryStatus {
    EntryStatus {
        needs_human: vec![NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "https://***@github.com/old/site".into(),
            },
            expected: "git@github.com:me/site".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::OutsideRepoFile,
            },
        }],
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::HostKey,
            message: "Host key verification failed.".into(),
        }),
        ..entry("site", follow("main"))
    }
}

// --- unregistered ---

fn stray(
    dir: &str,
    origin: Option<&str>,
    owned: bool,
    kind: UnregisteredKind,
) -> UnregisteredClone {
    UnregisteredClone {
        dir: dir.into(),
        origin: origin.map(str::to_owned),
        owned,
        kind,
    }
}

fn moved(blocked_by: Option<RepairBlock>, exit_noise: Option<&str>) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: "app".into(),
        blocked_by,
        exit_noise: exit_noise.map(str::to_owned),
    }
}

/// Every `UnregisteredKind` and `RepairBlock`, over each ownership.
fn unregistered() -> Vec<UnregisteredClone> {
    let app_origin = Some("git@github.com:me/app");
    vec![
        stray(
            "b-copy",
            app_origin,
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: Some(path("b-moved")),
            },
        ),
        stray("b-moved", app_origin, true, moved(None, Some("/home/me/y"))),
        stray(
            "c-copy",
            app_origin,
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: None,
            },
        ),
        stray(
            "claimed",
            app_origin,
            true,
            moved(
                Some(RepairBlock::ClaimedDir {
                    git_dir: git_dir("claimed"),
                }),
                None,
            ),
        ),
        stray(
            "lib",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Clone,
        ),
        stray(
            "lib-feat",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Worktree,
        ),
        stray(
            "mine",
            Some("git@github.com:me/mine"),
            true,
            UnregisteredKind::Clone,
        ),
        stray(
            "rel",
            app_origin,
            true,
            moved(
                Some(RepairBlock::RelativeGitdir {
                    git_dir: git_dir("k"),
                }),
                None,
            ),
        ),
        stray(
            "rewrites",
            app_origin,
            true,
            moved(
                Some(RepairBlock::Rewrites {
                    path: path("q"),
                    git_dir: git_dir("q"),
                }),
                None,
            ),
        ),
        stray(
            "scratch",
            None,
            false,
            UnregisteredKind::OrphanedWorktree {
                entry: "app".into(),
            },
        ),
        stray(
            "wa",
            app_origin,
            true,
            moved(
                Some(RepairBlock::Swapped {
                    git_dir: git_dir("wa"),
                    with: "wb".into(),
                }),
                None,
            ),
        ),
    ]
}

// --- the error document ---

fn repo_name(key: &str) -> EntryName {
    EntryName {
        kind: EntryKind::Repo,
        key: key.into(),
    }
}

/// Every `RegistryIssue`.
fn registry_issues() -> Vec<RegistryIssue> {
    vec![
        RegistryIssue::RepoNotOwned {
            key: "kit".into(),
            account: "sveltejs".into(),
        },
        RegistryIssue::ForkNotOwned { key: "wpt".into() },
        RegistryIssue::DirNotAName {
            entry: repo_name("app"),
            dir: "../app".into(),
        },
        RegistryIssue::DirClaimedTwice {
            dir: "app".into(),
            first: repo_name("app"),
            second: EntryName {
                kind: EntryKind::Reference,
                key: "app-ref".into(),
            },
        },
        RegistryIssue::KeyInBoth { key: "gro".into() },
        RegistryIssue::KeyIsOtherDir {
            key: "site".into(),
            entry: repo_name("www"),
        },
        RegistryIssue::UnknownCheckoutRef {
            key: "app".into(),
            field: CheckoutList::Requires,
            target: "nope".into(),
        },
        RegistryIssue::SelfRef {
            key: "app".into(),
            field: CheckoutList::Consults,
        },
        RegistryIssue::RequiresAndConsults {
            key: "app".into(),
            target: "gro".into(),
        },
    ]
}
