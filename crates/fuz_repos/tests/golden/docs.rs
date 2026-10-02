//! The hand-built documents the goldens serialize: each report, outcome, and
//! error document, over the fixed clock and workspace.

use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote, Primary, at_rest};
use fuz_repos::error::Error;
use fuz_repos::git::MIN_GIT_VERSION;
use fuz_repos::registry::{CheckoutList, EntryKind, EntryName, RegistryIssue, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    BranchOutcome, BranchSync, BranchSyncHold, CheckoutPush, CloneOutcome, CloneSyncHold,
    EntryStatus, EntrySync, FetchOutcome, NoUpstreamWhy, PushOutcome, PushReport, RebasePush,
    RebaseRefusal, RepairBlock, Sessions, StatusReport, SyncReport, UnregisteredClone,
    UnregisteredKind,
};
use fuz_repos::sessions::{Session, SessionSource, Unavailable};
use fuz_repos::state::{
    BranchHold, BranchNeedsHuman, BranchStatus, Checkout, CleanupReason, CloneHold, CloneRecipe,
    CloneVerdict, GitDirHolds, Head, InProgressOp, Layout, Presence, ProbeError, ProbeErrorKind,
    Prune, PruneLoss, RefreshHold, RefreshVerdict, Relation, SyncAction, Uncommitted, UnprobedWhy,
    UnprobedWorktree, UnprobedWorktreeStatus, Verdict,
};
use std::path::PathBuf;

use crate::invariants;

/// The fixed clock every timestamp is built from.
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

const WORKSPACE: &str = "/home/me/dev";

// --- the documents ---

/// A whole-workspace run under `--fetch`: every entry shape, every fetch
/// failure and visibility check, and the unregistered scan.
pub fn status_report_doc() -> StatusReport {
    let mut unscoped = unscoped_sessions();
    // a session whose dir was deleted from under it: `webref()`'s clone
    // would land where it works
    unscoped.push(Session::at(
        1500,
        0,
        path("webref/src"),
        SessionSource::SessionFile,
    ));
    let mut entries = vec![
        app(),
        fuz_app(),
        blog(),
        archived(),
        test262(),
        corpora(),
        spec(),
        zzz(),
        missing(),
        webref(),
        twin("gro"),
        renamed(),
        guide(),
        not_a_repo(),
        partial(),
        forge(),
        gro(),
        zap(),
        site(),
        mdz(),
        fuz_code(),
        fuz_docs(),
        tsv(),
        tsv_fuz_dev(),
        fuz_css(),
        fuz_ui(),
        uz(),
        pushy(),
        fetchy(),
        sparse_fork(),
        renamed_default(),
        almanac(),
        journal(),
        merged_in(),
        released(),
        shared(),
    ];
    entries.extend(probe_failures());
    report(
        true,
        Sessions::Available { unscoped },
        entries,
        Some(unregistered()),
    )
}

/// A run narrowed by targets, local refs only: the scan, the fetch, and
/// the visibility check didn't run; busy detection was unavailable, so
/// every action is held. The targets name every entry: a third-party
/// reference among them is refreshed, or held for its origin, and a pin
/// refused.
pub fn targeted_doc() -> StatusReport {
    report(
        false,
        Sessions::Unavailable {
            reason: foreign_domain(),
        },
        vec![
            EntryStatus {
                needs_human: vec![NeedsHuman::OriginMismatch {
                    origin: OriginRemote::Missing,
                    expected: "git@github.com:me/gro".into(),
                    fix: OriginFix::Add,
                }],
                ..entry("gro", Some("main"))
            },
            EntryStatus {
                branches: vec![BranchStatus {
                    unique_commits: 1,
                    worktree: Some(path("fuz_util")),
                    ..branch(
                        "main",
                        Some("origin/main"),
                        Relation::Ahead { commits: 1 },
                        Verdict::Held {
                            action: SyncAction::Push { commits: 1 },
                            by: BranchHold::BusyUnknown,
                        },
                    )
                }],
                ..entry("fuz_util", Some("main"))
            },
            // named: refreshed, previewed from local refs
            EntryStatus {
                refresh: Some(RefreshVerdict::Act),
                branches: vec![checked_out(
                    branch(
                        "main",
                        Some("origin/main"),
                        Relation::Behind { commits: 3 },
                        Verdict::Held {
                            action: SyncAction::FastForward { commits: 3 },
                            by: BranchHold::BusyUnknown,
                        },
                    ),
                    "typescript",
                )],
                ..third_party("typescript", Some("main"))
            },
            // named, its origin a fork: held, never fetched
            EntryStatus {
                refresh: Some(RefreshVerdict::Held {
                    by: RefreshHold::Entry,
                }),
                needs_human: vec![NeedsHuman::OriginMismatch {
                    origin: OriginRemote::Url {
                        url: "https://github.com/me/html".into(),
                    },
                    expected: "https://github.com/them/html".into(),
                    fix: OriginFix::SetUrl,
                }],
                // held, so compared against no remote: its local work alone
                branches: vec![BranchStatus {
                    unique_commits: 2,
                    ..branch("notes", None, Relation::Untracked, Verdict::LocalOnly)
                }],
                ..third_party("html", Some("main"))
            },
            // named, its origin the repo over SSH: held, never fetched
            EntryStatus {
                refresh: Some(RefreshVerdict::Held {
                    by: RefreshHold::OriginNotHttps,
                }),
                needs_human: vec![NeedsHuman::OriginNotHttps {
                    fetch_url: "git@github.com:them/lit".into(),
                    expected: "https://github.com/them/lit".into(),
                    fix: Some(OriginFix::SetUrl),
                }],
                // held, so compared against no remote: no local work to keep
                branches: vec![],
                ..third_party("lit", Some("main"))
            },
            // named, an `insteadOf` rewriting its HTTPS origin to SSH
            EntryStatus {
                refresh: Some(RefreshVerdict::Held {
                    by: RefreshHold::OriginNotHttps,
                }),
                needs_human: vec![NeedsHuman::OriginNotHttps {
                    fetch_url: "git@github.com:them/dom".into(),
                    expected: "https://github.com/them/dom".into(),
                    fix: None,
                }],
                // held, so compared against no remote: no local work to keep
                branches: vec![],
                ..third_party("dom", Some("main"))
            },
            // named: a pin refuses
            EntryStatus {
                kind: EntryKind::Reference,
                visibility: None,
                ci: false,
                pinned: true,
                refresh: Some(RefreshVerdict::Held {
                    by: RefreshHold::Pinned,
                }),
                checkouts: vec![primary("wpt", on("fork"))],
                branches: vec![checked_out(
                    branch(
                        "fork",
                        Some("origin/fork"),
                        Relation::Behind { commits: 2 },
                        Verdict::Held {
                            action: SyncAction::FastForward { commits: 2 },
                            by: BranchHold::Pinned,
                        },
                    ),
                    "wpt",
                )],
                ..entry("wpt", Some("fork"))
            },
        ],
        None,
    )
}

/// A `repos push --new-branch` of several targets: every outcome and fetch
/// outcome, each branch's verdict the one the outcome follows from — a
/// linked worktree's branch among them, beside its primary's — and each way
/// a diverged branch's rebase and the push after it can go.
pub fn push_report_doc() -> PushReport {
    let oid = |c: char| c.to_string().repeat(40);
    let push = |commits| SyncAction::Push { commits };
    let fetched = FetchOutcome::Fetched;
    let target = |key: &str, checkout: String, branch: Option<&str>, fetch, outcome| CheckoutPush {
        key: key.into(),
        checkout,
        branch: branch.map(str::to_owned),
        fetch,
        outcome,
    };
    let ahead = |name: &str, commits, verdict| BranchStatus {
        unique_commits: commits,
        ..branch(
            name,
            Some(&format!("origin/{name}")),
            Relation::Ahead { commits },
            verdict,
        )
    };
    let wt = "/home/me/wt/app-feat".to_owned();
    let app = EntryStatus {
        checkouts: vec![
            primary("app", on("main")),
            Checkout {
                path: wt.clone(),
                busy: vec![Session::at(4242, 0, wt.clone(), SessionSource::SessionFile)],
                working: vec![Session::at(4242, 0, wt.clone(), SessionSource::SessionFile)],
                ..linked(wt.clone(), on("feat"))
            },
        ],
        branches: vec![
            checked_out(ahead("main", 1, Verdict::Act { action: push(1) }), "app"),
            BranchStatus {
                worktree: Some(wt.clone()),
                ..ahead(
                    "feat",
                    2,
                    Verdict::Held {
                        action: push(2),
                        by: BranchHold::Busy,
                    },
                )
            },
        ],
        ..entry("app", Some("main"))
    };
    let in_sync = entry("blog", Some("main"));
    let behind = EntryStatus {
        branches: vec![checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Behind { commits: 2 },
                Verdict::Act {
                    action: SyncAction::FastForward { commits: 2 },
                },
            ),
            "site",
        )],
        ..entry("site", Some("main"))
    };
    let diverged = EntryStatus {
        branches: vec![BranchStatus {
            unique_commits: 1,
            worktree: Some(path("zap")),
            ..branch(
                "main",
                Some("origin/main"),
                Relation::Diverged {
                    ahead: 1,
                    behind: 1,
                },
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            )
        }],
        ..entry("zap", Some("main"))
    };
    // on a branch with no upstream: `main` beside it checked out nowhere
    let topic = |key: &str| EntryStatus {
        checkouts: vec![primary(key, on("topic"))],
        branches: vec![
            branch(
                "main",
                Some("origin/main"),
                Relation::InSync,
                Verdict::Quiet,
            ),
            BranchStatus {
                unique_commits: 1,
                worktree: Some(path(key)),
                ..branch("topic", None, Relation::Untracked, Verdict::LocalOnly)
            },
        ],
        ..entry(key, Some("main"))
    };
    let no_upstream = topic("gro");
    let detached = EntryStatus {
        checkouts: vec![primary("mdz", Head::Detached { commit: oid('d') })],
        branches: vec![branch(
            "main",
            Some("origin/main"),
            Relation::InSync,
            Verdict::Quiet,
        )],
        needs_human: vec![NeedsHuman::UnexpectedDetached {
            checkout: path("mdz"),
        }],
        ..entry("mdz", Some("main"))
    };
    let failure = RemoteFailure::Unreachable {
        cause: UnreachableCause::Connection,
        message: "ssh: connect to host github.com port 22: Connection timed out".into(),
    };
    let fetch_failed = EntryStatus {
        fetched_at: None,
        fetch_error: Some(failure.clone()),
        branches: vec![checked_out(
            ahead(
                "main",
                1,
                Verdict::Held {
                    action: push(1),
                    by: BranchHold::FetchFailed,
                },
            ),
            "forge",
        )],
        ..entry("forge", Some("main"))
    };
    let refused = EntryStatus {
        branches: vec![checked_out(
            ahead("main", 1, Verdict::Act { action: push(1) }),
            "tsv",
        )],
        ..entry("tsv", Some("main"))
    };
    // on `main` with no commit yet: no ref, so no branch, said missing
    let unborn = EntryStatus {
        branches: vec![],
        needs_human: vec![NeedsHuman::DefaultBranchMissing {
            branch: "main".into(),
        }],
        ..entry("uz", Some("main"))
    };
    // no upstream: created on origin, and found there at another commit
    let created = topic("fuz_ui");
    let exists = topic("fuz_css");
    // no upstream on origin, and none `--new-branch` creates: merged and
    // deleted on origin, the entry's own branch gone, tracking another
    // remote
    let on_branch = |key: &str, b: BranchStatus| EntryStatus {
        checkouts: vec![primary(key, on(&b.name))],
        branches: vec![checked_out(b, key)],
        ..entry(key, Some("main"))
    };
    let merged = on_branch(
        "fuz_code",
        branch(
            "done",
            Some("origin/done"),
            Relation::Gone,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: None,
            },
        ),
    );
    let default_gone = EntryStatus {
        needs_human: vec![NeedsHuman::DefaultBranchGone {
            branch: "main".into(),
        }],
        ..on_branch(
            "fuz_docs",
            BranchStatus {
                unique_commits: 1,
                ..branch(
                    "main",
                    Some("origin/main"),
                    Relation::Gone,
                    Verdict::LocalOnly,
                )
            },
        )
    };
    let elsewhere = on_branch(
        "fuz_blog",
        BranchStatus {
            unique_commits: 1,
            ..branch(
                "fork",
                Some("upstream/fork"),
                Relation::Untracked,
                Verdict::LocalOnly,
            )
        },
    );
    // the registry's branch, diverged: rebased, each way its push can go,
    // refused by its replay, and held in a dirty checkout
    let rebase = |ahead, behind| Verdict::Act {
        action: SyncAction::Rebase { ahead, behind },
    };
    let rebased = |key: &str, push| {
        target(
            key,
            path(key),
            Some("main"),
            FetchOutcome::Fetched,
            PushOutcome::Rebased {
                from: oid('1'),
                to: oid('2'),
                onto: oid('3'),
                push,
            },
        )
    };
    let refused_replay = |key: &str, why| {
        target(
            key,
            path(key),
            Some("main"),
            FetchOutcome::Fetched,
            PushOutcome::RebaseRefused { why },
        )
    };
    let rebases = vec![
        self::diverged("almanac", 2, 3, rebase(2, 3)),
        self::diverged("atlas", 1, 1, rebase(1, 1)),
        self::diverged("ledger", 1, 2, rebase(1, 2)),
        self::diverged("gazette", 3, 1, rebase(3, 1)),
        self::diverged("digest", 1, 1, rebase(1, 1)),
        self::diverged("primer", 1, 1, rebase(1, 1)),
        self::diverged("memoir", 2, 1, rebase(2, 1)),
        journal(),
    ];
    let mut entries = vec![
        app,
        in_sync,
        behind,
        diverged,
        no_upstream,
        detached,
        missing(),
        fetch_failed,
        refused,
        unborn,
        created,
        exists,
        merged,
        default_gone,
        elsewhere,
    ];
    entries.extend(rebases);
    let status = report(
        true,
        Sessions::Available { unscoped: vec![] },
        entries,
        None,
    );
    PushReport::new(
        status,
        vec![
            rebased("almanac", RebasePush::Pushed),
            rebased("atlas", RebasePush::AlreadyThere),
            rebased(
                "ledger",
                RebasePush::Held {
                    by: BranchSyncHold::Changed,
                },
            ),
            rebased(
                "gazette",
                RebasePush::PushFailed {
                    failure: RemoteFailure::Rejected {
                        reason: "pre-receive hook declined".into(),
                        message: None,
                    },
                },
            ),
            rebased(
                "digest",
                RebasePush::Failed {
                    message: "fatal: unable to access the pack".into(),
                },
            ),
            refused_replay("primer", RebaseRefusal::Conflicts),
            refused_replay(
                "memoir",
                RebaseRefusal::AlreadyUpstream { commit: oid('4') },
            ),
            target(
                "journal",
                path("journal"),
                Some("main"),
                fetched.clone(),
                PushOutcome::Held {
                    by: BranchSyncHold::DirtyCheckout,
                },
            ),
            target(
                "app",
                path("app"),
                Some("main"),
                fetched.clone(),
                PushOutcome::Pushed {
                    from: oid('a'),
                    to: oid('b'),
                },
            ),
            target(
                "app",
                wt,
                Some("feat"),
                fetched.clone(),
                PushOutcome::Held {
                    by: BranchSyncHold::Busy,
                },
            ),
            target(
                "blog",
                path("blog"),
                Some("main"),
                fetched.clone(),
                PushOutcome::InSync,
            ),
            target(
                "site",
                path("site"),
                Some("main"),
                fetched.clone(),
                PushOutcome::NotAhead,
            ),
            target(
                "zap",
                path("zap"),
                Some("main"),
                fetched.clone(),
                PushOutcome::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            ),
            target(
                "gro",
                path("gro"),
                Some("topic"),
                fetched.clone(),
                PushOutcome::NoUpstream {
                    why: NoUpstreamWhy::Creatable,
                },
            ),
            target(
                "mdz",
                path("mdz"),
                None,
                fetched.clone(),
                PushOutcome::Detached,
            ),
            target(
                "blake3",
                path("blake3"),
                None,
                FetchOutcome::NotFetched,
                PushOutcome::Unread,
            ),
            target(
                "forge",
                path("forge"),
                Some("main"),
                FetchOutcome::Failed { failure },
                PushOutcome::Held {
                    by: BranchSyncHold::FetchFailed,
                },
            ),
            target(
                "tsv",
                path("tsv"),
                Some("main"),
                fetched.clone(),
                PushOutcome::PushFailed {
                    failure: RemoteFailure::Rejected {
                        reason: "protected branch hook declined".into(),
                        message: Some(
                            "GH006: Protected branch update failed for refs/heads/main.".into(),
                        ),
                    },
                },
            ),
            target(
                "uz",
                path("uz"),
                Some("main"),
                fetched.clone(),
                PushOutcome::Failed {
                    message: "main has no commit to push".into(),
                },
            ),
            target(
                "fuz_ui",
                path("fuz_ui"),
                Some("topic"),
                fetched.clone(),
                PushOutcome::Created { to: oid('c') },
            ),
            target(
                "fuz_css",
                path("fuz_css"),
                Some("topic"),
                fetched.clone(),
                PushOutcome::RemoteBranchExists { at: oid('e') },
            ),
            target(
                "fuz_code",
                path("fuz_code"),
                Some("done"),
                fetched.clone(),
                PushOutcome::NoUpstream {
                    why: NoUpstreamWhy::Merged,
                },
            ),
            target(
                "fuz_docs",
                path("fuz_docs"),
                Some("main"),
                fetched.clone(),
                PushOutcome::NoUpstream {
                    why: NoUpstreamWhy::DefaultGone,
                },
            ),
            target(
                "fuz_blog",
                path("fuz_blog"),
                Some("fork"),
                fetched,
                PushOutcome::NoUpstream {
                    why: NoUpstreamWhy::OtherUpstream,
                },
            ),
        ],
    )
}

/// Every state of busy detection, one per report.
pub fn sessions_doc() -> Vec<Sessions> {
    vec![
        Sessions::Available {
            unscoped: unscoped_sessions(),
        },
        Sessions::Available { unscoped: vec![] },
        Sessions::Unavailable {
            reason: Unavailable::HomeUnknown,
        },
        Sessions::Unavailable {
            reason: Unavailable::RelativeConfigDir {
                path: "claude".into(),
            },
        },
        Sessions::Unavailable {
            reason: Unavailable::Unreadable {
                path: "/home/me/.claude/sessions".into(),
                error: "Permission denied (os error 13)".into(),
            },
        },
        Sessions::Unavailable {
            reason: Unavailable::Unparseable {
                path: "/home/me/.claude/sessions/4242.json".into(),
                error: "missing field `procStart` at line 1 column 80".into(),
            },
        },
        Sessions::Unavailable {
            reason: foreign_domain(),
        },
    ]
}

fn foreign_domain() -> Unavailable {
    Unavailable::ForeignPidDomain {
        path: "/home/me/.claude/sessions/77.json".into(),
        pid_domain: "linux:0123456789abcdef0123456789abcdef:pid:[4026532001]".into(),
        source: SessionSource::SessionFile,
    }
}

/// Live sessions in no checkout: one at the workspace root, a background
/// worker outside it.
fn unscoped_sessions() -> Vec<Session> {
    vec![
        Session::at(1200, 0, WORKSPACE.into(), SessionSource::SessionFile),
        Session::at(
            1300,
            0,
            "/home/me/notes".into(),
            SessionSource::RosterWorker,
        ),
    ]
}

/// A `sync --references` run over the whole workspace: every outcome,
/// fetch outcome, and hold, each branch's verdict the one the outcome
/// carries out.
pub fn sync_report_doc() -> SyncReport {
    let ff = |commits| SyncAction::FastForward { commits };
    let push = |commits| SyncAction::Push { commits };
    let act = |action| Verdict::Act { action };
    let held_by = |action, by| Verdict::Held { action, by };
    let behind = |commits| Relation::Behind { commits };
    let up = |name: &str| format!("origin/{name}");
    let oid = |c: char| c.to_string().repeat(40);
    // (name, relation, verdict, where it's checked out, outcome)
    let app_branches: Vec<(&str, Relation, Verdict, Option<String>, BranchOutcome)> = vec![
        (
            "main",
            behind(2),
            act(ff(2)),
            Some(path("app")),
            BranchOutcome::FastForwarded {
                from: oid('a'),
                to: oid('b'),
            },
        ),
        (
            "feat",
            behind(1),
            act(ff(1)),
            None,
            BranchOutcome::Failed {
                action: ff(1),
                message: "git rejected moving refs/heads/feat: not a fast-forward".into(),
            },
        ),
        (
            "late",
            behind(1),
            act(ff(1)),
            None,
            BranchOutcome::Held {
                action: ff(1),
                by: BranchSyncHold::Changed,
            },
        ),
        (
            "ahead",
            Relation::Ahead { commits: 3 },
            act(push(3)),
            None,
            BranchOutcome::Pushed {
                from: oid('e'),
                to: oid('f'),
            },
        ),
        // deleted on the remote after the fetch: the lease refused it
        (
            "fresh",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            None,
            BranchOutcome::Held {
                action: push(1),
                by: BranchSyncHold::Changed,
            },
        ),
        (
            "guarded",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            None,
            BranchOutcome::PushFailed {
                failure: RemoteFailure::Rejected {
                    reason: "protected branch hook declined".into(),
                    message: Some(
                        "GH006: Protected branch update failed for refs/heads/guarded.".into(),
                    ),
                },
            },
        ),
        // a push URL set between classifying and pushing
        (
            "rerouted",
            Relation::Ahead { commits: 1 },
            act(push(1)),
            None,
            BranchOutcome::Held {
                action: push(1),
                by: BranchSyncHold::PushUrl,
            },
        ),
        (
            "dirty",
            behind(1),
            held_by(ff(1), BranchHold::DirtyCheckout),
            Some(path("app-dirty")),
            BranchOutcome::Held {
                action: ff(1),
                by: BranchSyncHold::DirtyCheckout,
            },
        ),
        (
            "busy",
            behind(1),
            held_by(ff(1), BranchHold::Busy),
            Some(path("app-busy")),
            BranchOutcome::Held {
                action: ff(1),
                by: BranchSyncHold::Busy,
            },
        ),
        (
            "unseen",
            behind(1),
            act(ff(1)),
            None,
            BranchOutcome::Held {
                action: ff(1),
                by: BranchSyncHold::BusyUnknown,
            },
        ),
        (
            "usb",
            behind(4),
            held_by(ff(4), BranchHold::UnprobedWorktree),
            Some("/media/usb/app".into()),
            BranchOutcome::Held {
                action: ff(4),
                by: BranchSyncHold::UnprobedWorktree,
            },
        ),
        (
            "twin",
            behind(1),
            held_by(ff(1), BranchHold::SeveralCheckouts),
            Some(path("app-twin")),
            BranchOutcome::Held {
                action: ff(1),
                by: BranchSyncHold::SeveralCheckouts,
            },
        ),
        (
            "arc",
            Relation::Diverged {
                ahead: 1,
                behind: 1,
            },
            Verdict::NeedsHuman {
                reason: BranchNeedsHuman::Diverged,
            },
            None,
            BranchOutcome::NeedsHuman {
                reason: BranchNeedsHuman::Diverged,
            },
        ),
        (
            "done",
            Relation::InSync,
            Verdict::Quiet,
            None,
            BranchOutcome::Untouched,
        ),
    ];
    let mut app = entry("app", Some("main"));
    app.checkouts.extend([
        Checkout {
            uncommitted: Uncommitted {
                unstaged: 1,
                ..Uncommitted::default()
            },
            ..linked(path("app-dirty"), on("dirty"))
        },
        Checkout {
            busy: vec![Session::at(
                4242,
                0,
                path("app-busy"),
                SessionSource::SessionFile,
            )],
            ..linked(path("app-busy"), on("busy"))
        },
        // one branch on HEAD in two (`worktree add -f`)
        linked(path("app-twin"), on("twin")),
        linked(path("app-twin2"), on("twin")),
    ]);
    app.unprobed_worktrees = vec![
        usb(),
        // gone, recorded where the missing `stray` would be cloned
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(NOTHING_HELD),
                ..unprobed(
                    "app",
                    "stray",
                    Some(Head::Detached { commit: oid('7') }),
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: vec![PruneLoss::DetachedHead],
            }),
            busy: vec![],
        },
    ];
    app.branches.clear();
    let mut app_sync = Vec::new();
    for (name, relation, verdict, worktree, outcome) in app_branches {
        app.branches.push(BranchStatus {
            worktree,
            ..branch(name, Some(&up(name)), relation, verdict)
        });
        app_sync.push(BranchSync {
            name: name.into(),
            outcome,
            repeats: None,
        });
    }
    // a linked worktree of app's, its own entry: app acted on `main` for
    // the repo, checked out in app's own dir — the main worktree, as this
    // entry's probe lists it
    let app_wt = EntryStatus {
        dir: "app-wt".into(),
        url: "https://github.com/me/app".into(),
        checkouts: vec![
            Checkout {
                linked: true,
                ..primary("app-wt", on("wt"))
            },
            Checkout {
                linked: false,
                ..linked(path("app"), on("main"))
            },
        ],
        branches: vec![
            checked_out(
                branch("main", Some("origin/main"), behind(2), act(ff(2))),
                "app",
            ),
            checked_out(
                branch("wt", Some("origin/wt"), Relation::InSync, Verdict::Quiet),
                "app-wt",
            ),
        ],
        ..entry("app_wt", Some("main"))
    };
    let app_wt_sync = vec![
        BranchSync {
            name: "main".into(),
            outcome: BranchOutcome::FastForwarded {
                from: oid('a'),
                to: oid('b'),
            },
            repeats: Some("app".into()),
        },
        BranchSync {
            name: "wt".into(),
            outcome: BranchOutcome::Untouched,
            repeats: None,
        },
    ];
    let one = |e: &EntryStatus, relation, verdict, outcome| {
        let mut e = e.clone();
        let on_main = matches!(&e.checkouts[0].head, Head::Branch { name } if name == "main");
        e.branches = vec![BranchStatus {
            worktree: on_main.then(|| e.checkouts[0].path.clone()),
            ..branch("main", Some("origin/main"), relation, verdict)
        }];
        let sync = vec![BranchSync {
            name: "main".into(),
            outcome,
            repeats: None,
        }];
        (e, sync)
    };
    let (blog, blog_sync) = one(
        &EntryStatus {
            checkouts: vec![Checkout {
                in_progress: Some(InProgressOp::Merge),
                ..primary("blog", on("main"))
            }],
            needs_human: vec![NeedsHuman::OperationInProgress {
                checkout: path("blog"),
                op: InProgressOp::Merge,
            }],
            ..entry("blog", Some("main"))
        },
        behind(1),
        held_by(ff(1), BranchHold::Entry),
        BranchOutcome::Held {
            action: ff(1),
            by: BranchSyncHold::Entry,
        },
    );
    let forge_failure = RemoteFailure::Unreachable {
        cause: UnreachableCause::Auth,
        message: "git@github.com: Permission denied (publickey).".into(),
    };
    let (forge, forge_sync) = one(
        &EntryStatus {
            fetched_at: None,
            fetch_error: Some(forge_failure.clone()),
            ..entry("fuz_forge", Some("main"))
        },
        behind(2),
        held_by(ff(2), BranchHold::FetchFailed),
        BranchOutcome::Held {
            action: ff(2),
            by: BranchSyncHold::FetchFailed,
        },
    );
    // shallow: a branch with nothing local moved to the fetched tip
    let (corpora, corpora_sync) = one(
        &EntryStatus {
            layout: Some(shallow_layout()),
            ..entry("corpora", Some("main"))
        },
        Relation::Shallow,
        act(SyncAction::Move),
        BranchOutcome::Moved {
            from: oid('c'),
            to: oid('d'),
        },
    );
    let (spec, spec_sync) = one(
        &EntryStatus {
            kind: EntryKind::Reference,
            branch: None,
            pinned: true,
            ..entry("spec", None)
        },
        behind(1),
        held_by(ff(1), BranchHold::Pinned),
        BranchOutcome::Held {
            action: ff(1),
            by: BranchSyncHold::Pinned,
        },
    );
    // diverged, the registry's branch: rebased onto the fetched tip, then
    // pushed — or not; or the replay refused, nothing moved
    let rebase = SyncAction::Rebase {
        ahead: 2,
        behind: 3,
    };
    let rebased = |key: &str, push| {
        let outcome = BranchOutcome::Rebased {
            from: oid('1'),
            to: oid('2'),
            onto: oid('3'),
            push,
        };
        one(
            &diverged(key, 2, 3, act(rebase)),
            Relation::Diverged {
                ahead: 2,
                behind: 3,
            },
            act(rebase),
            outcome,
        )
    };
    let refused = |key: &str, why| {
        one(
            &diverged(key, 2, 3, act(rebase)),
            Relation::Diverged {
                ahead: 2,
                behind: 3,
            },
            act(rebase),
            BranchOutcome::RebaseRefused { why },
        )
    };
    let rebases = vec![
        rebased("fieldbook", RebasePush::Pushed),
        rebased("journal", RebasePush::AlreadyThere),
        rebased(
            "atlas",
            RebasePush::Held {
                by: BranchSyncHold::Changed,
            },
        ),
        rebased(
            "ledger",
            RebasePush::PushFailed {
                failure: RemoteFailure::Rejected {
                    reason: "protected branch hook declined".into(),
                    message: Some(
                        "GH006: Protected branch update failed for refs/heads/main.".into(),
                    ),
                },
            },
        ),
        rebased(
            "almanac",
            RebasePush::Failed {
                message: "git send-pack reported nothing for refs/heads/main".into(),
            },
        ),
        refused("cord", RebaseRefusal::Conflicts),
        refused("dealt", RebaseRefusal::AlreadyUpstream { commit: oid('4') }),
    ];
    // `--references`: fetched over HTTPS, fast-forwarded where clean;
    // never pushed, so a branch ahead is local-only work
    let lib = EntryStatus {
        refresh: Some(RefreshVerdict::Act),
        branches: vec![
            checked_out(
                branch("main", Some("origin/main"), behind(1), act(ff(1))),
                "lib",
            ),
            BranchStatus {
                unique_commits: 2,
                ..branch(
                    "audit",
                    Some("origin/audit"),
                    Relation::Ahead { commits: 2 },
                    Verdict::LocalOnly,
                )
            },
        ],
        ..third_party("lib", None)
    };
    let lib_sync = vec![
        BranchSync {
            name: "main".into(),
            outcome: BranchOutcome::FastForwarded {
                from: oid('5'),
                to: oid('6'),
            },
            repeats: None,
        },
        BranchSync {
            name: "audit".into(),
            outcome: BranchOutcome::Untouched,
            repeats: None,
        },
    ];
    let broken = EntryStatus {
        probe_error: Some(ProbeError::new(
            ProbeErrorKind::GitFailed,
            "git status failed (128): error: bad tree object HEAD",
        )),
        checkouts: vec![],
        branches: vec![],
        fetched_at: None,
        ..entry("broken", Some("main"))
    };
    // missing: cloned, held as classified or at the moment of cloning,
    // failed at the remote, failed placing it, held for a session working
    // where it would land
    let stray = EntryStatus {
        key: "stray".into(),
        dir: "stray".into(),
        url: "https://github.com/me/stray".into(),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/stray".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: CloneHold::UnprobedWorktree,
        }),
        ..missing()
    };
    // a session whose dir was deleted from under it: `webref()`'s clone
    // would land where it works
    let unscoped = vec![Session::at(
        1500,
        0,
        path("webref/src"),
        SessionSource::SessionFile,
    )];
    let (rebase_entries, rebase_syncs): (Vec<EntryStatus>, Vec<EntrySync>) = rebases
        .into_iter()
        .map(|(e, branches)| {
            let sync = EntrySync {
                key: e.key.clone(),
                fetch: FetchOutcome::Fetched,
                clone: None,
                branches,
            };
            (e, sync)
        })
        .unzip();
    let mut entries = vec![
        app,
        app_wt,
        blog,
        forge,
        corpora,
        spec,
        lib,
        missing(),
        stray,
        twin("app"),
        renamed(),
        missing_reference("wpt", None),
        missing_reference("html", None),
        missing_reference("dom", None),
        webref(),
        broken,
    ];
    entries.extend(rebase_entries);
    // no targets: the scan ran first
    let status = report(
        true,
        Sessions::Available { unscoped },
        entries,
        Some(vec![renamed_old()]),
    );
    let sync = |key: &str, fetch, branches| EntrySync {
        key: key.into(),
        fetch,
        clone: None,
        branches,
    };
    let cloned = |key: &str, clone| EntrySync {
        key: key.into(),
        fetch: FetchOutcome::NotFetched,
        clone: Some(clone),
        branches: vec![],
    };
    let mut synced = vec![
        sync("app", FetchOutcome::Fetched, app_sync),
        sync("app_wt", FetchOutcome::Fetched, app_wt_sync),
        sync("blog", FetchOutcome::Fetched, blog_sync),
        sync(
            "fuz_forge",
            FetchOutcome::Failed {
                failure: forge_failure,
            },
            forge_sync,
        ),
        sync("corpora", FetchOutcome::Fetched, corpora_sync),
        sync("spec", FetchOutcome::NotFetched, spec_sync),
        sync("lib", FetchOutcome::Fetched, lib_sync),
        cloned(
            "blake3",
            CloneOutcome::Cloned {
                branch: "main".into(),
                head: oid('c'),
            },
        ),
        cloned(
            "stray",
            CloneOutcome::Held {
                by: CloneSyncHold::UnprobedWorktree,
            },
        ),
        cloned(
            "twin",
            CloneOutcome::Held {
                by: CloneSyncHold::Entry,
            },
        ),
        cloned(
            "renamed",
            CloneOutcome::Held {
                by: CloneSyncHold::Entry,
            },
        ),
        // a dir made at the path since the probe
        cloned(
            "wpt",
            CloneOutcome::Held {
                by: CloneSyncHold::Changed,
            },
        ),
        cloned(
            "html",
            CloneOutcome::CloneFailed {
                failure: RemoteFailure::RepoNotFound {
                    message: "remote: Repository not found.".into(),
                },
            },
        ),
        cloned(
            "dom",
            CloneOutcome::Failed {
                message: format!(
                    "can't move the clone into {WORKSPACE}/dom: Permission denied (os \
                         error 13)"
                ),
            },
        ),
        cloned(
            "webref",
            CloneOutcome::Held {
                by: CloneSyncHold::Busy,
            },
        ),
        sync("broken", FetchOutcome::Fetched, vec![]),
    ];
    synced.extend(rebase_syncs);
    SyncReport::new(status, synced)
}

/// Every fatal error `repos status --json` can print (`error_reports`).
pub fn status_errors() -> Vec<Error> {
    let registry = || PathBuf::from(format!("{WORKSPACE}/repos.toml"));
    let denied = || std::io::Error::other("Permission denied (os error 13)");
    vec![
        Error::ReferencesWithTargets,
        Error::RootNotFound {
            root: PathBuf::from("/home/me/nowhere"),
        },
        Error::RegistryNotFound {
            start: PathBuf::from("/home/me"),
        },
        Error::RootInEntry {
            root: PathBuf::from(path("meta")),
            registry: PathBuf::from(path("meta/repos.toml")),
            key: "meta".into(),
        },
        Error::RegistryRead {
            path: registry(),
            source: denied(),
        },
        Error::RegistryParse {
            path: registry(),
            message: "TOML parse error at line 4, column 1: invalid table header".into(),
        },
        // the kind with the richest payload: every issue
        Error::RegistryInvalid {
            path: registry(),
            issues: registry_issues(),
        },
        Error::GitNotFound,
        Error::GitTooOld {
            found: "2.39.5".into(),
            required: MIN_GIT_VERSION,
        },
        Error::UnknownEntry {
            name: "mta".into(),
            suggestions: vec!["meta".into()],
        },
        Error::Io {
            context: format!("failed to list the workspace root {WORKSPACE}"),
            source: denied(),
        },
    ]
}

// --- entries ---

/// A report of `entries` — and of the unregistered scan's dirs, when it ran
/// — each entry's `at_rest` decided from what it carries (`at_rest`, as
/// `classify` decides it): its primary checkout, and its branches, compared
/// against origin when owned or refreshed. Checked for its structure
/// (`invariants`).
pub fn report(
    fetched: bool,
    sessions: Sessions,
    mut entries: Vec<EntryStatus>,
    unregistered: Option<Vec<UnregisteredClone>>,
) -> StatusReport {
    for e in &mut entries {
        let tracked = e.writable || e.refresh == Some(RefreshVerdict::Act);
        e.at_rest = e.checkouts.first().map(|c| {
            assert!(c.primary, "{}: the primary first", e.key);
            let primary = Primary {
                head: &c.head,
                uncommitted: c.uncommitted,
                in_progress: c.in_progress,
            };
            at_rest(e.branch.as_deref(), &primary, tracked, &e.branches)
        });
    }
    let mut report = StatusReport::new(
        WORKSPACE.into(),
        format!("{WORKSPACE}/repos.toml"),
        fetched,
        sessions,
        entries,
    );
    report.unregistered = unregistered;
    invariants::assert_structure(&report);
    report
}

fn path(dir: &str) -> String {
    format!("{WORKSPACE}/{dir}")
}

fn shallow_layout() -> Layout {
    Layout {
        shallow: true,
        ..Layout::default()
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
        busy: vec![],
        working: vec![],
    }
}

/// A clean linked worktree at `path`, with nothing to say.
fn linked(path: String, head: Head) -> Checkout {
    Checkout {
        path,
        primary: false,
        linked: true,
        ..primary("", head)
    }
}

pub fn on(name: &str) -> Head {
    Head::Branch { name: name.into() }
}

/// A present, owned, public repo on `main`, unpinned, fetched a day ago,
/// with nothing to say: its one branch, `main`, in sync with origin's.
pub fn entry(key: &str, followed: Option<&str>) -> EntryStatus {
    EntryStatus {
        key: key.into(),
        kind: EntryKind::Repo,
        dir: key.into(),
        url: format!("https://github.com/me/{key}"),
        writable: true,
        archived: false,
        visibility: Some(Visibility::Public),
        ci: true,
        branch: followed.map(str::to_owned),
        pinned: false,
        refresh: None,
        presence: Presence::Present,
        clone: None,
        layout: Some(Layout::default()),
        checkouts: vec![primary(key, on("main"))],
        branches: vec![checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::InSync,
                Verdict::Quiet,
            ),
            key,
        )],
        at_rest: None,
        stashes: 0,
        fetched_at: Some(NOW - DAY),
        needs_human: vec![],
        probe_error: None,
        unprobed_worktrees: vec![],
        fetch_error: None,
        visibility_check: None,
    }
}

/// A third-party reference present on `main`, `key` of `them`'s: never
/// fetched unless a run refreshes it, so by default its branches are its
/// local work alone — none.
pub fn third_party(key: &str, followed: Option<&str>) -> EntryStatus {
    EntryStatus {
        kind: EntryKind::Reference,
        url: format!("https://github.com/them/{key}"),
        writable: false,
        visibility: None,
        ci: false,
        branches: vec![],
        ..entry(key, followed)
    }
}

/// A branch checked out nowhere.
pub fn branch(
    name: &str,
    upstream: Option<&str>,
    relation: Relation,
    verdict: Verdict,
) -> BranchStatus {
    BranchStatus {
        name: name.into(),
        upstream: upstream.map(str::to_owned),
        worktree: None,
        symref: None,
        unique_commits: 0,
        newest_commit_at: NOW - 2 * DAY,
        relation,
        verdict,
    }
}

/// `b`, checked out in the entry dir `dir`.
pub fn checked_out(b: BranchStatus, dir: &str) -> BranchStatus {
    BranchStatus {
        worktree: Some(path(dir)),
        ..b
    }
}

/// The busiest repo, each action its own: every verdict kind; a push, a
/// fast-forward, and every hold but `Entry` (`blog()`, `fuz_app()`),
/// `Pinned` (`spec()`), `PushUrl` (`pushy()`), `FetchFailed` (`forge()`),
/// `SeveralCheckouts` (`gro()`), and `BusyUnknown` (the targeted document);
/// linked worktrees, a live session working in one; gone worktrees, one a
/// session still works in, one moved into the workspace root, one where
/// `guide()` would be cloned; and a git dir no worktree list names, a
/// session working through it.
fn app() -> EntryStatus {
    let mut e = entry("app", Some("main"));
    e.stashes = 2;
    e.checkouts[0].uncommitted = Uncommitted {
        staged: 1,
        unstaged: 2,
        untracked: 3,
        conflicted: 0,
    };
    e.checkouts.push(Checkout {
        uncommitted: Uncommitted {
            unstaged: 1,
            ..Uncommitted::default()
        },
        ..linked(path("app-feat"), on("feat"))
    });
    e.checkouts.push(Checkout {
        // launched at the workspace root, its process since moved in
        busy: vec![Session {
            process_cwd: Some(path("app/.claude/worktrees/agent/src")),
            ..Session::at(4242, 0, WORKSPACE.into(), SessionSource::SessionFile)
        }],
        ..linked(path("app/.claude/worktrees/agent"), on("agent"))
    });
    e.checkouts.push(Checkout {
        submodules: Some(false),
        ..linked("/home/me/wt/app-old".into(), on("old"))
    });
    e.branches = vec![
        BranchStatus {
            unique_commits: 2,
            ..checked_out(
                branch(
                    "main",
                    Some("origin/main"),
                    Relation::Ahead { commits: 2 },
                    Verdict::Act {
                        action: SyncAction::Push { commits: 2 },
                    },
                ),
                "app",
            )
        },
        branch(
            "next",
            Some("origin/next"),
            Relation::Behind { commits: 2 },
            Verdict::Act {
                action: SyncAction::FastForward { commits: 2 },
            },
        ),
        checked_out(
            branch(
                "feat",
                Some("origin/feat"),
                Relation::Behind { commits: 1 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 1 },
                    by: BranchHold::DirtyCheckout,
                },
            ),
            "app-feat",
        ),
        BranchStatus {
            unique_commits: 1,
            ..checked_out(
                branch(
                    "agent",
                    Some("origin/agent"),
                    Relation::Ahead { commits: 1 },
                    Verdict::Held {
                        action: SyncAction::Push { commits: 1 },
                        by: BranchHold::Busy,
                    },
                ),
                "app/.claude/worktrees/agent",
            )
        },
        BranchStatus {
            worktree: Some("/media/usb/app".into()),
            ..branch(
                "usb",
                Some("origin/usb"),
                Relation::Behind { commits: 4 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 4 },
                    by: BranchHold::UnprobedWorktree,
                },
            )
        },
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
        // on HEAD in a gone worktree
        checked_out(
            branch(
                "gone",
                Some("origin/gone"),
                Relation::InSync,
                Verdict::Quiet,
            ),
            "app-gone",
        ),
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
        // an alias of `main`: never acted on
        BranchStatus {
            symref: Some("refs/heads/main".into()),
            ..branch("m", None, Relation::Untracked, Verdict::Quiet)
        },
    ];
    e.needs_human = vec![NeedsHuman::UnlistedGitDir {
        git_dir: "/home/me/hand/.git".into(),
        head: Some(Head::Branch {
            name: "other".into(),
        }),
        busy: vec![Session::at(
            1400,
            0,
            "/home/me/hand".into(),
            SessionSource::SessionFile,
        )],
    }];
    e.unprobed_worktrees = vec![
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(NOTHING_HELD),
                ..unprobed("app", "app-gone", Some(on("gone")), UnprobedWhy::Prunable)
            },
            prune: Some(Prune::Safe),
            // a session still in its deleted dir
            busy: vec![Session {
                worktree: Some(path("app-gone")),
                ..Session::at(4343, 0, path("app-gone/src"), SessionSource::RosterWorker)
            }],
        },
        moved_worktree("app-b", &["b-copy", "b-moved"]),
        moved_worktree("app-d", &["d-moved"]),
        usb(),
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(NOTHING_HELD),
                ..unprobed(
                    "app",
                    "guide",
                    Some(Head::Detached {
                        commit: "4567456745674567456745674567456745674567".into(),
                    }),
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: vec![PruneLoss::DetachedHead],
            }),
            busy: vec![],
        },
    ];
    e
}

/// A gone worktree of app's on the branch `usb`, kept by git for its lock:
/// on unmounted media.
fn usb() -> UnprobedWorktreeStatus {
    UnprobedWorktreeStatus {
        worktree: UnprobedWorktree {
            path: "/media/usb/app".into(),
            locked: true,
            ..unprobed("app", "usb", Some(on("usb")), UnprobedWhy::Missing)
        },
        prune: None,
        busy: vec![],
    }
}

/// A gone worktree of app's whose files the unregistered scan found at the
/// workspace root, in `to`.
fn moved_worktree(dir: &str, to: &[&str]) -> UnprobedWorktreeStatus {
    UnprobedWorktreeStatus {
        worktree: UnprobedWorktree {
            holds: Some(NOTHING_HELD),
            ..unprobed("app", dir, Some(on(dir)), UnprobedWhy::Prunable)
        },
        prune: Some(Prune::Moved {
            to: to.iter().map(|d| (*d).to_owned()).collect(),
        }),
        busy: vec![],
    }
}

/// A gone worktree's git dir holding nothing of its own.
const NOTHING_HELD: GitDirHolds = GitDirHolds {
    submodules: false,
    worktree_refs: false,
    staged: Some(false),
};

/// `key`'s own git dir for its worktree `id`.
fn git_dir(key: &str, id: &str) -> String {
    path(&format!("{key}/.git/worktrees/{id}"))
}

fn unprobed(key: &str, dir: &str, head: Option<Head>, why: UnprobedWhy) -> UnprobedWorktree {
    UnprobedWorktree {
        path: path(dir),
        git_dir: Some(git_dir(key, dir)),
        head,
        locked: false,
        in_progress: None,
        why,
        holds: None,
    }
}

/// Stopped mid-way everywhere, so held whole (`BranchHold::Entry`): a
/// bisect in its primary, a cherry-pick in a linked worktree, a revert in a
/// gone one, and a worktree git dir that can't be read; its fetch failed
/// too. One of its worktree git dirs names its worktree relatively, so no
/// path of the repo is certain: every gone worktree loses what that hides.
/// Between them its gone worktrees lose every `PruneLoss`, and two have
/// HEADs no one can read, which might be on any branch.
fn fuz_app() -> EntryStatus {
    let mut e = entry("fuz_app", Some("main"));
    e.fetch_error = Some(RemoteFailure::Failed {
        message: "fatal: protocol error: bad line length character: Welc".into(),
    });
    e.checkouts[0].head = Head::Detached {
        commit: "fedcba9876543210fedcba9876543210fedcba98".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Bisect);
    e.checkouts.push(Checkout {
        uncommitted: Uncommitted {
            conflicted: 1,
            ..Uncommitted::default()
        },
        in_progress: Some(InProgressOp::CherryPick),
        locked: true,
        submodules: Some(true),
        ..linked(
            path("fuz_app-fix"),
            Head::Detached {
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
        )
    });
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 3 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 3 },
                by: BranchHold::Entry,
            },
        ),
        BranchStatus {
            unique_commits: 1,
            ..branch(
                "topic",
                Some("origin/topic"),
                Relation::Ahead { commits: 1 },
                Verdict::Held {
                    action: SyncAction::Push { commits: 1 },
                    by: BranchHold::Entry,
                },
            )
        },
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_app"),
            op: InProgressOp::Bisect,
        },
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_app-fix"),
            op: InProgressOp::CherryPick,
        },
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_app-spike"),
            op: InProgressOp::Revert,
        },
        NeedsHuman::WorktreeUnreadable {
            path: git_dir("fuz_app", "x"),
        },
    ];
    let relative = PruneLoss::RelativeGitdir {
        git_dir: git_dir("fuz_app", "k"),
    };
    let head_unknown = |dir| unprobed("fuz_app", dir, None, UnprobedWhy::Prunable);
    e.unprobed_worktrees = vec![
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                in_progress: Some(InProgressOp::Revert),
                holds: Some(GitDirHolds {
                    submodules: true,
                    worktree_refs: true,
                    staged: None,
                }),
                ..unprobed(
                    "fuz_app",
                    "fuz_app-spike",
                    Some(Head::Detached {
                        commit: "89abcdef0123456789abcdef0123456789abcdef".into(),
                    }),
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: vec![
                    PruneLoss::Operation {
                        op: InProgressOp::Revert,
                    },
                    PruneLoss::DetachedHead,
                    PruneLoss::Submodules,
                    PruneLoss::WorktreeRefs,
                    PruneLoss::StagedChanges,
                    relative.clone(),
                ],
            }),
            busy: vec![],
        },
        // listed by git, no git dir matching it: its branch deleted since
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                git_dir: None,
                ..unprobed(
                    "fuz_app",
                    "fuz_app-lost",
                    Some(on("spike")),
                    UnprobedWhy::Prunable,
                )
            },
            prune: Some(Prune::Loses {
                losses: vec![
                    PruneLoss::MissingBranch {
                        name: "spike".into(),
                    },
                    PruneLoss::UnmatchedGitDir,
                    relative.clone(),
                ],
            }),
            busy: vec![],
        },
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                holds: Some(GitDirHolds {
                    staged: None,
                    ..NOTHING_HELD
                }),
                ..head_unknown("fuz_app-blind")
            },
            prune: Some(Prune::Loses {
                losses: vec![PruneLoss::UnknownHead, relative],
            }),
            busy: vec![],
        },
        // a git dir git doesn't list, its `HEAD` unreadable
        UnprobedWorktreeStatus {
            worktree: UnprobedWorktree {
                path: git_dir("fuz_app", "x"),
                git_dir: None,
                ..unprobed(
                    "fuz_app",
                    "x",
                    None,
                    UnprobedWhy::Failed {
                        error: "not listed by git: reading HEAD: Permission denied".into(),
                    },
                )
            },
            prune: None,
            busy: vec![],
        },
    ];
    e
}

/// Origin drift, holding the entry's push, so never fetched; a merge in its
/// primary; a worktree whose path can't be resolved.
fn blog() -> EntryStatus {
    let mut e = entry("fuz_blog", Some("main"));
    e.url = "https://github.com/fuzdev/fuz_blog".into();
    e.checkouts[0].in_progress = Some(InProgressOp::Merge);
    e.branches = vec![BranchStatus {
        unique_commits: 2,
        ..checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Ahead { commits: 2 },
                Verdict::Held {
                    action: SyncAction::Push { commits: 2 },
                    by: BranchHold::Entry,
                },
            ),
            "fuz_blog",
        )
    }];
    let sealed = "/home/me/sealed/fuz_blog-wt";
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("fuz_blog"),
            op: InProgressOp::Merge,
        },
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "git@github.com:ryanatkn/fuz_blog".into(),
            },
            expected: "git@github.com:fuzdev/fuz_blog".into(),
            fix: OriginFix::SetUrl,
        },
        NeedsHuman::CheckoutUnresolvable {
            checkout: sealed.into(),
            path: sealed.into(),
            error: "Permission denied (os error 13)".into(),
        },
    ];
    e.unprobed_worktrees = vec![UnprobedWorktreeStatus {
        worktree: UnprobedWorktree {
            path: sealed.into(),
            ..unprobed(
                "fuz_blog",
                "fuz_blog-wt",
                Some(Head::Detached {
                    commit: "2345234523452345234523452345234523452345".into(),
                }),
                UnprobedWhy::Failed {
                    error: "Permission denied (os error 13)".into(),
                },
            )
        },
        prune: None,
        busy: vec![],
    }];
    e
}

/// Archived and private, no CI, with a commit ahead its host refuses, and
/// no `origin` — and anyone can read it.
fn archived() -> EntryStatus {
    let mut e = entry("old", Some("main"));
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
        ..checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Ahead { commits: 1 },
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::ArchivedAhead,
                },
            ),
            "old",
        )
    }];
    e
}

/// A third-party reference the run doesn't refresh, left at its HEAD:
/// shallow and sparse, detached mid-rebase, compared against no remote, so
/// only its branch with local work is listed.
fn test262() -> EntryStatus {
    let mut e = third_party("test262", None);
    e.url = "https://github.com/tc39/test262".into();
    e.layout = Some(Layout {
        shallow: true,
        sparse: true,
        ..Layout::default()
    });
    e.checkouts[0].head = Head::Detached {
        commit: "fedcba9876543210fedcba9876543210fedcba98".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Rebase);
    e.branches = vec![BranchStatus {
        unique_commits: 4,
        ..branch("audit", None, Relation::Untracked, Verdict::LocalOnly)
    }];
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

/// An owned repo cloned shallow by hand: a branch with nothing local moves
/// to the fetched tip, and is held where it's checked out dirty; one with
/// local work off the tip needs a person, and one with commits on the tip
/// pushes.
fn corpora() -> EntryStatus {
    let mut e = entry("corpora", Some("main"));
    e.layout = Some(shallow_layout());
    e.checkouts.push(Checkout {
        uncommitted: Uncommitted {
            unstaged: 2,
            ..Uncommitted::default()
        },
        ..linked(path("corpora-docs"), on("docs"))
    });
    e.branches = vec![
        checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Shallow,
                Verdict::Act {
                    action: SyncAction::Move,
                },
            ),
            "corpora",
        ),
        checked_out(
            branch(
                "docs",
                Some("origin/docs"),
                Relation::Shallow,
                Verdict::Held {
                    action: SyncAction::Move,
                    by: BranchHold::DirtyCheckout,
                },
            ),
            "corpora-docs",
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
            unique_commits: 1,
            ..branch(
                "patch",
                Some("origin/patch"),
                Relation::Ahead { commits: 1 },
                Verdict::Act {
                    action: SyncAction::Push { commits: 1 },
                },
            )
        },
    ];
    e
}

/// An owned fork kept as a reference, pinned on the branch it lives on and
/// behind a stale remote-tracking ref (the pin holds the fast-forward), with
/// a `git am` stopped mid-way and a valueless `origin` URL.
fn spec() -> EntryStatus {
    let mut e = entry("ecma262", Some("draft"));
    e.pinned = true;
    e.kind = EntryKind::Reference;
    e.visibility = None;
    e.checkouts[0].head = on("draft");
    e.checkouts[0].in_progress = Some(InProgressOp::Am);
    e.branches = vec![checked_out(
        branch(
            "draft",
            Some("origin/draft"),
            Relation::Behind { commits: 5 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 5 },
                by: BranchHold::Pinned,
            },
        ),
        "ecma262",
    )];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("ecma262"),
            op: InProgressOp::Am,
        },
        NeedsHuman::OriginMismatch {
            origin: OriginRemote::NoUrl,
            expected: "git@github.com:me/ecma262".into(),
            fix: OriginFix::ByHand {
                reason: OriginByHand::ValuelessUrl,
            },
        },
    ];
    e
}

/// Following `dev`, which is missing, detached where it shouldn't be, with a
/// sequencer stopped in it; `main`, with no upstream and nothing of its own,
/// reads merged.
fn zzz() -> EntryStatus {
    let mut e = entry("zzz", Some("dev"));
    e.fetched_at = None;
    e.fetch_error = Some(RemoteFailure::Unreachable {
        cause: UnreachableCause::Connection,
        message: "ssh: connect to host github.com port 22: Connection refused".into(),
    });
    e.checkouts[0].head = Head::Detached {
        commit: "00112233445566778899aabbccddeeff00112233".into(),
    };
    e.checkouts[0].in_progress = Some(InProgressOp::Sequencer);
    e.branches = vec![branch(
        "main",
        None,
        Relation::Untracked,
        Verdict::Cleanup {
            reason: CleanupReason::Merged,
            removable_worktree: None,
        },
    )];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: path("zzz"),
            op: InProgressOp::Sequencer,
        },
        NeedsHuman::DefaultBranchMissing {
            branch: "dev".into(),
        },
        NeedsHuman::UnexpectedDetached {
            checkout: path("zzz"),
        },
    ];
    e
}

/// Following `master`, whose upstream is gone from origin — the remote's
/// default renamed to `main`: a person's, never cleanup. Its fetch timed
/// out.
fn renamed_default() -> EntryStatus {
    let mut e = entry("mageguild", Some("master"));
    e.checkouts[0].head = on("master");
    e.branches = vec![checked_out(
        branch(
            "master",
            Some("origin/master"),
            Relation::Gone,
            Verdict::Quiet,
        ),
        "mageguild",
    )];
    e.needs_human = vec![NeedsHuman::DefaultBranchGone {
        branch: "master".into(),
    }];
    e.fetch_error = Some(RemoteFailure::TimedOut { after_secs: 120 });
    e
}

/// Not cloned yet: sync would clone it, over SSH on its branch.
pub fn missing() -> EntryStatus {
    EntryStatus {
        presence: Presence::Missing,
        clone: Some(CloneVerdict::Act {
            recipe: CloneRecipe {
                url: "git@github.com:me/blake3".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
        }),
        layout: None,
        checkouts: vec![],
        branches: vec![],
        fetched_at: None,
        ..entry("blake3", Some("main"))
    }
}

/// A missing third-party reference, `key`, cloned over HTTPS from the
/// remote's default branch, shallow and sparse — its clone `held` or not.
fn missing_reference(key: &str, held: Option<CloneHold>) -> EntryStatus {
    let recipe = CloneRecipe {
        url: format!("https://github.com/them/{key}"),
        branch: None,
        shallow: true,
        sparse: Some("css".into()),
    };
    EntryStatus {
        presence: Presence::Missing,
        clone: Some(match held {
            Some(by) => CloneVerdict::Held { recipe, by },
            None => CloneVerdict::Act { recipe },
        }),
        layout: None,
        checkouts: vec![],
        fetched_at: None,
        ..third_party(key, None)
    }
}

/// A missing reference a live session works in, its dir deleted from under
/// it (`status_report_doc`'s unscoped sessions): the clone would land
/// where it works.
fn webref() -> EntryStatus {
    missing_reference("webref", Some(CloneHold::Busy))
}

/// A missing entry naming `with`'s repo, held for a person: its dir may
/// have been a worktree of that repo.
fn twin(with: &str) -> EntryStatus {
    EntryStatus {
        key: "twin".into(),
        dir: "twin".into(),
        url: format!("https://github.com/me/{with}"),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: format!("git@github.com:me/{with}"),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: CloneHold::Entry,
        }),
        needs_human: vec![NeedsHuman::CloneSharesRepo { with: with.into() }],
        ..missing()
    }
}

/// A missing entry whose repo the unregistered dir `renamed-old` clones,
/// held for a person: likely its checkout under another name.
fn renamed() -> EntryStatus {
    EntryStatus {
        key: "renamed".into(),
        dir: "renamed".into(),
        url: "https://github.com/me/renamed".into(),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/renamed".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: CloneHold::Entry,
        }),
        needs_human: vec![NeedsHuman::ClonedUnregistered {
            dir: "renamed-old".into(),
        }],
        ..missing()
    }
}

/// A missing entry where git still records a gone worktree of app's
/// (`app()`): the clone would be taken for its files.
fn guide() -> EntryStatus {
    EntryStatus {
        key: "guide".into(),
        dir: "guide".into(),
        url: "https://github.com/me/guide".into(),
        clone: Some(CloneVerdict::Held {
            recipe: CloneRecipe {
                url: "git@github.com:me/guide".into(),
                branch: Some("main".into()),
                shallow: false,
                sparse: None,
            },
            by: CloneHold::UnprobedWorktree,
        }),
        ..missing()
    }
}

/// The unregistered dir `renamed()` is held by: a clone of its repo.
fn renamed_old() -> UnregisteredClone {
    stray(
        "renamed-old",
        Some("git@github.com:me/renamed"),
        true,
        UnregisteredKind::Clone,
    )
}

/// A dir that holds no repo.
fn not_a_repo() -> EntryStatus {
    EntryStatus {
        presence: Presence::NotARepo,
        layout: None,
        checkouts: vec![],
        branches: vec![],
        fetched_at: None,
        needs_human: vec![NeedsHuman::NotARepo {
            detail: "empty directory".into(),
        }],
        ..entry("goblins", Some("main"))
    }
}

/// A partial clone whose probe failed on a missing object: the layout read
/// first stands, the rest is incomplete.
fn partial() -> EntryStatus {
    EntryStatus {
        layout: Some(Layout {
            partial_filter: Some("tree:0".into()),
            ..Layout::default()
        }),
        checkouts: vec![],
        branches: vec![],
        fetched_at: None,
        probe_error: Some(ProbeError::new(
            ProbeErrorKind::GitFailed,
            "git status failed (128): error: bad tree object HEAD",
        )),
        fetch_error: Some(RemoteFailure::RepoNotFound {
            message: "ERROR: Repository not found.".into(),
        }),
        ..entry("wpt", Some("main"))
    }
}

/// A probe failed every other way it can (`partial()` is a git call that
/// failed), each with the layout read before the failure, if any: looking
/// at the path and reading the config and fetch URL come first.
fn probe_failures() -> Vec<EntryStatus> {
    let failed = |key: &str, kind, message: String, layout| EntryStatus {
        layout,
        checkouts: vec![],
        branches: vec![],
        fetched_at: None,
        probe_error: Some(ProbeError::new(kind, message)),
        ..entry(key, Some("main"))
    };
    let status_args = "status --porcelain=v2 --branch --show-stash --no-ahead-behind \
                       --no-renames --untracked-files=normal -z";
    vec![
        failed(
            "vault",
            ProbeErrorKind::PathUnreadable,
            format!(
                "can't look up {}: Permission denied (os error 13)",
                path("vault")
            ),
            None,
        ),
        failed(
            "notes",
            ProbeErrorKind::NonUtf8Path,
            format!("non-UTF-8 path {}", path("notes")),
            None,
        ),
        failed(
            "scratch",
            ProbeErrorKind::UnexpectedOutput,
            format!("rev-parse: unexpected output `{}/.git`", path("scratch")),
            None,
        ),
        failed(
            "dotfiles",
            ProbeErrorKind::ConfigUnreadable,
            "config failed: fatal: bad config line 7 in file .git/config".into(),
            None,
        ),
        failed(
            "mirror",
            ProbeErrorKind::FetchUrlUnreadable,
            "fetch URL: git ls-remote --get-url origin timed out after 60s".into(),
            None,
        ),
        failed(
            "kiln",
            ProbeErrorKind::GitNotRun,
            "failed to run git: Resource temporarily unavailable (os error 11)".into(),
            Some(Layout::default()),
        ),
        failed(
            "monorepo",
            ProbeErrorKind::GitTimedOut,
            format!("git {status_args} timed out after 60s"),
            Some(Layout::default()),
        ),
        failed(
            "relay",
            ProbeErrorKind::PushUrlsUnreadable,
            "push URLs: git remote get-url --push --all origin timed out after 60s".into(),
            Some(Layout::default()),
        ),
    ]
}

/// Private as declared; its key refused.
/// A fetch refused by the host: its branch behind stays put
/// (`BranchHold::FetchFailed`).
fn forge() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetched_at: None,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Auth,
            message: "git@github.com: Permission denied (publickey).".into(),
        }),
        visibility_check: Some(VisibilityCheck::Private),
        branches: vec![checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Behind { commits: 2 },
                Verdict::Held {
                    action: SyncAction::FastForward { commits: 2 },
                    by: BranchHold::FetchFailed,
                },
            ),
            "fuz_forge",
        )],
        ..entry("fuz_forge", Some("main"))
    }
}

/// A branch on HEAD in two clean checkouts (`worktree add -f`): its
/// fast-forward held (`BranchHold::SeveralCheckouts`).
fn gro() -> EntryStatus {
    let mut e = entry("gro", Some("main"));
    e.checkouts.push(linked(path("gro-twin"), on("main")));
    e.branches = vec![checked_out(
        branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 3 },
            Verdict::Held {
                action: SyncAction::FastForward { commits: 3 },
                by: BranchHold::SeveralCheckouts,
            },
        ),
        "gro",
    )];
    e
}

/// Private, with the network down: its host not found, the check timed out.
fn zap() -> EntryStatus {
    EntryStatus {
        visibility: Some(Visibility::Private),
        ci: false,
        fetched_at: None,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::Dns,
            message: "ssh: Could not resolve hostname github.com: Temporary failure in name \
                      resolution"
                .into(),
        }),
        visibility_check: Some(VisibilityCheck::Unknown {
            failure: RemoteFailure::TimedOut { after_secs: 120 },
        }),
        ..entry("zap", Some("main"))
    }
}

/// An old `origin` URL beside a mirror: never fetched.
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
        ..entry("mdz", Some("main"))
    }
}

/// An entry whose `main` diverged from origin's, `ahead` and `behind`, with
/// `verdict`.
fn diverged(key: &str, ahead: u32, behind: u32, verdict: Verdict) -> EntryStatus {
    let mut e = entry(key, Some("main"));
    e.branches = vec![BranchStatus {
        unique_commits: ahead,
        ..checked_out(
            branch(
                "main",
                Some("origin/main"),
                Relation::Diverged { ahead, behind },
                verdict,
            ),
            key,
        )
    }];
    e
}

/// Its `main` diverged from origin's, the registry's branch, clean: sync
/// rebases it.
fn almanac() -> EntryStatus {
    let action = SyncAction::Rebase {
        ahead: 2,
        behind: 3,
    };
    diverged("almanac", 2, 3, Verdict::Act { action })
}

/// Its `main` diverged, in a dirty checkout: the rebase held.
fn journal() -> EntryStatus {
    let action = SyncAction::Rebase {
        ahead: 1,
        behind: 4,
    };
    let by = BranchHold::DirtyCheckout;
    let mut e = diverged("journal", 1, 4, Verdict::Held { action, by });
    e.checkouts[0].uncommitted = Uncommitted {
        untracked: 1,
        ..Uncommitted::default()
    };
    e
}

/// Its `main` diverged, one of the commits it's ahead by held by another
/// remote branch (a pushed `feat`, merged here): a person's.
fn shared() -> EntryStatus {
    let reason = BranchNeedsHuman::DivergedPublished;
    let mut e = diverged("shared", 2, 1, Verdict::NeedsHuman { reason });
    e.branches[0].unique_commits = 1;
    e
}

/// Its `main` diverged, a merge among its local-only commits: a person's.
fn merged_in() -> EntryStatus {
    let reason = BranchNeedsHuman::DivergedMerge;
    diverged("merged_in", 3, 1, Verdict::NeedsHuman { reason })
}

/// Its `main` diverged, a tag on one of its local-only commits: a person's.
fn released() -> EntryStatus {
    let reason = BranchNeedsHuman::DivergedTagged;
    diverged("released", 1, 2, Verdict::NeedsHuman { reason })
}

/// A branch deleted on the remote, named by one of several fetch refspecs.
fn fuz_code() -> EntryStatus {
    EntryStatus {
        fetched_at: None,
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/attrs".into(),
            fix: RefGoneFix::UnsetRefspec {
                pattern: r"^\+?refs/heads/attrs(:|$)".into(),
            },
        }),
        ..entry("fuz_code", Some("main"))
    }
}

/// A host key ssh doesn't trust.
fn fuz_docs() -> EntryStatus {
    EntryStatus {
        fetched_at: None,
        fetch_error: Some(RemoteFailure::Unreachable {
            cause: UnreachableCause::HostKey,
            message: "Host key verification failed.".into(),
        }),
        ..entry("fuz_docs", Some("main"))
    }
}

/// A single-branch clone whose own branch, the registry's, is gone: no
/// branch to name.
fn tsv() -> EntryStatus {
    EntryStatus {
        fetched_at: None,
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/main".into(),
            fix: RefGoneFix::SetBranches { branch: None },
        }),
        ..entry("tsv", Some("main"))
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
        ..entry("fuz_css", Some("main"))
    }
}

/// A legacy remotes file that can't be read: `status --fetch` doesn't
/// fetch.
fn fuz_ui() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::LegacyRemotesUnreadable {
            path: path("fuz_ui/.git/remotes/old"),
        }),
        ..entry("fuz_ui", Some("main"))
    }
}

/// A refspec writing tags: `status --fetch` doesn't fetch it.
fn tsv_fuz_dev() -> EntryStatus {
    EntryStatus {
        fetch_error: Some(RemoteFailure::RefspecOutsideOrigin {
            refspec: "+refs/tags/*:refs/tags/*".into(),
        }),
        ..entry("tsv.fuz.dev", Some("main"))
    }
}

/// A missing ref no refspec in the repo's config names; the branch it
/// follows has no upstream, and a commit on no remote.
fn uz() -> EntryStatus {
    EntryStatus {
        fetched_at: None,
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "typecheck-arc".into(),
            fix: RefGoneFix::ByHand,
        }),
        branches: vec![BranchStatus {
            unique_commits: 1,
            ..checked_out(
                branch("main", None, Relation::Untracked, Verdict::LocalOnly),
                "uz",
            )
        }],
        needs_human: vec![NeedsHuman::DefaultBranchNoUpstream {
            branch: "main".into(),
        }],
        ..entry("uz", Some("main"))
    }
}

/// Pushes that can't go through origin: a push URL rewritten to another
/// repo, holding the one ahead (`PushUrl`, named before the failed fetch),
/// and a branch ahead of `origin/HEAD`, whose ref on origin isn't a branch.
/// Its fetch names a branch gone from the remote, in the only refspec its
/// own config holds (a global one maps the rest): point origin at the
/// registry's branch.
fn pushy() -> EntryStatus {
    let push = SyncAction::Push { commits: 1 };
    EntryStatus {
        branches: vec![
            BranchStatus {
                unique_commits: 1,
                ..checked_out(
                    branch(
                        "main",
                        Some("origin/main"),
                        Relation::Ahead { commits: 1 },
                        Verdict::Held {
                            action: push,
                            by: BranchHold::PushUrl,
                        },
                    ),
                    "pushy",
                )
            },
            BranchStatus {
                unique_commits: 1,
                ..branch(
                    "tip",
                    Some("origin/HEAD"),
                    Relation::Ahead { commits: 1 },
                    Verdict::NeedsHuman {
                        reason: BranchNeedsHuman::UpstreamNotABranch,
                    },
                )
            },
        ],
        needs_human: vec![NeedsHuman::PushUrlMismatch {
            push_urls: vec![
                "git@github.com:me/pushy".into(),
                "https://***@mirror.example.com/me/pushy".into(),
            ],
            expected: "git@github.com:me/pushy".into(),
        }],
        fetched_at: None,
        fetch_error: Some(RemoteFailure::RefGone {
            refname: "refs/heads/dev".into(),
            fix: RefGoneFix::SetBranches {
                branch: Some("main".into()),
            },
        }),
        ..entry("pushy", Some("main"))
    }
}

/// A fetch an `insteadOf` rewrite sends to another repo: held whole, never
/// fetched, the rewrite to change.
fn fetchy() -> EntryStatus {
    EntryStatus {
        needs_human: vec![NeedsHuman::FetchUrlMismatch {
            fetch_url: "git@github.com:me/mirror".into(),
            expected: "git@github.com:me/fetchy".into(),
            fix: None,
        }],
        fetched_at: None,
        ..entry("fetchy", Some("main"))
    }
}

/// A partial clone whose origin is the repo over plain `http://`, which its
/// checkouts' lazy fetch may not take: held whole, never fetched.
fn sparse_fork() -> EntryStatus {
    EntryStatus {
        layout: Some(Layout {
            sparse: true,
            partial_filter: Some("blob:none".into()),
            ..Layout::default()
        }),
        needs_human: vec![NeedsHuman::FetchUrlMismatch {
            fetch_url: "http://github.com/me/sparse_fork".into(),
            expected: "git@github.com:me/sparse_fork".into(),
            fix: Some(OriginFix::SetUrl),
        }],
        ..entry("sparse_fork", Some("main"))
    }
}

/// An old origin, with a token in it (redacted), set in global config:
/// never fetched.
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
        ..entry("site", Some("main"))
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

fn moved(
    entry: &str,
    blocked_by: Option<RepairBlock>,
    exit_noise: Option<&str>,
) -> UnregisteredKind {
    UnregisteredKind::MovedWorktree {
        entry: entry.into(),
        blocked_by,
        exit_noise: exit_noise.map(str::to_owned),
    }
}

/// Every `UnregisteredKind` and `RepairBlock`, over each ownership: app's
/// moved worktrees, each blocked its own way (or not), `fuz_app`'s blocked by
/// its relative `gitdir` and zzz's by an unreadable one, as every moved
/// worktree of those repos would be.
fn unregistered() -> Vec<UnregisteredClone> {
    let app_origin = Some("git@github.com:me/app");
    let app_blocked =
        |dir: &str, block| stray(dir, app_origin, true, moved("app", Some(block), None));
    let shares = |dir: &str, with: Option<&str>| {
        stray(
            dir,
            app_origin,
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: with.map(path),
            },
        )
    };
    vec![
        stray(
            ".blake3.repos-clone-4242-0123456789abcdef",
            Some("git@github.com:me/blake3"),
            true,
            UnregisteredKind::UnfinishedClone,
        ),
        // two copies of app's gone `app-b`
        shares("b-copy", Some("b-moved")),
        shares("b-moved", Some("b-copy")),
        shares("c-copy", None),
        app_blocked(
            "claimed",
            RepairBlock::ClaimedDir {
                git_dir: git_dir("app", "claimed"),
            },
        ),
        stray(
            "d-moved",
            app_origin,
            true,
            moved("app", None, Some("/home/me/y")),
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
            Some("git@github.com:me/fuz_app"),
            true,
            moved(
                "fuz_app",
                Some(RepairBlock::RelativeGitdir {
                    git_dir: git_dir("fuz_app", "k"),
                }),
                None,
            ),
        ),
        renamed_old(),
        app_blocked(
            "rewrites",
            RepairBlock::Rewrites {
                path: path("q"),
                git_dir: git_dir("app", "q"),
            },
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
            "unreadable",
            Some("git@github.com:me/zzz"),
            true,
            moved(
                "zzz",
                Some(RepairBlock::UnreadableGitdir {
                    git_dir: git_dir("zzz", "u"),
                }),
                None,
            ),
        ),
        app_blocked("v\u{fffd}", RepairBlock::NonUtf8Path),
        app_blocked(
            "w-nul",
            RepairBlock::NulInGitdir {
                git_dir: git_dir("app", "w-nul"),
            },
        ),
        app_blocked(
            "wa",
            RepairBlock::Swapped {
                git_dir: git_dir("app", "wa"),
                with: "wb".into(),
            },
        ),
        app_blocked(
            "wb",
            RepairBlock::Swapped {
                git_dir: git_dir("app", "wb"),
                with: "wa".into(),
            },
        ),
    ]
}

// --- the error documents ---

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
