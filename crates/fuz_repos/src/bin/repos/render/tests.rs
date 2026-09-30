use fuz_repos::registry::{EntryKind, Visibility};
use fuz_repos::report::{BranchSync, FetchOutcome};
use fuz_repos::state::{
    AtRest, Checkout, CloneRecipe, InProgressOp, Layout, UnprobedWorktree, UnprobedWorktreeStatus,
};

use super::labels::group_digits;
use super::summary::{Items, render_group};
use super::*;

/// Where a test entry's checkout lives and who moves its HEAD.
#[derive(Debug, Clone, Copy)]
enum Mode<'a> {
    Follow(&'a str),
    Pinned,
    /// Pinned, its checkout living on the branch.
    PinnedOn(&'a str),
    Head,
}

fn entry(key: &str, mode: Mode<'_>, head: &str) -> EntryStatus {
    let (branch, pinned) = match mode {
        Mode::Follow(branch) => (Some(branch.to_owned()), false),
        Mode::Pinned => (None, true),
        Mode::PinnedOn(branch) => (Some(branch.to_owned()), true),
        Mode::Head => (None, false),
    };
    let on_branch = branch.as_ref().map(|b| b == head);
    EntryStatus {
        key: key.into(),
        kind: EntryKind::Repo,
        dir: key.into(),
        url: format!("https://github.com/me/{key}"),
        writable: true,
        archived: false,
        visibility: Some(Visibility::Public),
        ci: true,
        branch,
        pinned,
        refresh: None,
        presence: Presence::Present,
        clone: None,
        layout: Some(Layout {
            shallow: false,
            sparse: false,
            partial_filter: None,
        }),
        checkouts: vec![Checkout {
            path: format!("/home/me/dev/{key}"),
            primary: true,
            head: Head::Branch { name: head.into() },
            uncommitted: Uncommitted::default(),
            in_progress: None,
            locked: false,
            linked: false,
            submodules: None,
            busy: vec![],
            working: vec![],
        }],
        branches: vec![],
        at_rest: Some(AtRest {
            on_branch,
            clean: true,
            idle: true,
            followed: None,
        }),
        stashes: 0,
        fetched_at: Some(NOW - 3 * 3600),
        needs_human: vec![],
        probe_error: None,
        unprobed_worktrees: vec![],
        fetch_error: None,
        visibility_check: None,
    }
}

/// A missing owned repo on `main`, which sync would clone.
fn missing(key: &str) -> EntryStatus {
    let mut e = entry(key, main(), "main");
    e.presence = Presence::Missing;
    e.layout = None;
    e.checkouts.clear();
    e.at_rest = None;
    e.clone = Some(CloneVerdict::Act {
        recipe: CloneRecipe {
            url: format!("git@github.com:me/{key}"),
            branch: Some("main".into()),
            shallow: false,
            sparse: None,
        },
    });
    e
}

const fn main() -> Mode<'static> {
    Mode::Follow("main")
}

fn branch(
    name: &str,
    upstream: Option<&str>,
    relation: Relation,
    unique: u32,
    verdict: Verdict,
) -> BranchStatus {
    BranchStatus {
        name: name.into(),
        upstream: upstream.map(str::to_owned),
        worktree: None,
        symref: None,
        unique_commits: unique,
        newest_commit_at: NOW - 2 * 86400,
        relation,
        verdict,
    }
}

const fn act(action: SyncAction) -> Verdict {
    Verdict::Act { action }
}

const fn needs(reason: BranchNeedsHuman) -> Verdict {
    Verdict::NeedsHuman { reason }
}

const fn cleanup(reason: CleanupReason) -> Verdict {
    Verdict::Cleanup {
        reason,
        removable_worktree: None,
    }
}

/// A clean linked worktree at `path` on `head`.
fn linked(path: &str, head: &str) -> Checkout {
    Checkout {
        path: path.into(),
        primary: false,
        head: Head::Branch { name: head.into() },
        uncommitted: Uncommitted::default(),
        in_progress: None,
        locked: false,
        linked: true,
        submodules: Some(false),
        busy: vec![],
        working: vec![],
    }
}

/// An unprobed worktree at `path`.
fn unprobed(path: &str, branch: Option<&str>, why: UnprobedWhy) -> UnprobedWorktree {
    UnprobedWorktree {
        path: path.into(),
        git_dir: None,
        head: branch.map_or_else(
            || UnprobedHead::Detached {
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            |name| UnprobedHead::Branch {
                name: name.to_owned(),
            },
        ),
        locked: false,
        in_progress: None,
        why,
        holds: None,
    }
}

/// An unprobed worktree as the report carries it.
const fn status(worktree: UnprobedWorktree, prune: Option<Prune>) -> UnprobedWorktreeStatus {
    UnprobedWorktreeStatus {
        worktree,
        prune,
        busy: Vec::new(),
    }
}

fn report(entries: Vec<EntryStatus>) -> StatusReport {
    StatusReport::new(
        "/home/me/dev".into(),
        "/home/me/dev/repos.toml".into(),
        false,
        Sessions::Available { unscoped: vec![] },
        entries,
    )
}

const NOW: u64 = 1_800_000_000;

const VIEW: View<'static> = View {
    home: Some("/home/me"),
    now: NOW,
    width: DEFAULT_WIDTH,
    color: false,
};

/// Each push outcome in `repos push`'s summary, grouped as sync's are,
/// with the hints for what isn't the push's to do.
#[test]
fn pushes_read_as_what_the_push_did() {
    use fuz_repos::report::{CheckoutPush, PushOutcome};
    let push = |commits| SyncAction::Push { commits };
    let with = |key: &str, head: &str, b: BranchStatus| {
        let mut e = entry(key, main(), head);
        e.branches = vec![b];
        e
    };
    let ahead = |name: &str, n, verdict| {
        branch(
            name,
            Some("origin/x"),
            Relation::Ahead { commits: n },
            n,
            verdict,
        )
    };
    let mut detached = entry("mdz", main(), "main");
    detached.checkouts[0].head = Head::Detached {
        commit: "d".repeat(40),
    };
    let mut gone = missing("gone");
    gone.clone = None;
    let r = report(vec![
        with(
            "app",
            "main",
            ahead("main", 2, Verdict::Act { action: push(2) }),
        ),
        with(
            "blog",
            "feat",
            ahead(
                "feat",
                1,
                Verdict::Held {
                    action: push(1),
                    by: HeldBy::Busy,
                },
            ),
        ),
        with(
            "site",
            "main",
            branch(
                "main",
                Some("origin/main"),
                Relation::Behind { commits: 3 },
                0,
                Verdict::Act {
                    action: SyncAction::FastForward { commits: 3 },
                },
            ),
        ),
        with(
            "zap",
            "main",
            branch(
                "main",
                Some("origin/main"),
                Relation::Diverged {
                    ahead: 1,
                    behind: 2,
                },
                1,
                Verdict::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            ),
        ),
        with(
            "gro",
            "topic",
            branch("topic", None, Relation::Untracked, 1, Verdict::LocalOnly),
        ),
        with(
            "uz",
            "main",
            branch(
                "main",
                Some("origin/main"),
                Relation::InSync,
                0,
                Verdict::Quiet,
            ),
        ),
        detached,
        gone,
        with(
            "tsv",
            "main",
            ahead("main", 1, Verdict::Act { action: push(1) }),
        ),
    ]);
    let target = |key: &str, branch: Option<&str>, outcome| CheckoutPush {
        key: key.into(),
        checkout: format!("/home/me/dev/{key}"),
        branch: branch.map(str::to_owned),
        fetch: FetchOutcome::Fetched,
        outcome,
    };
    let pushed = PushReport::new(
        r,
        vec![
            target(
                "app",
                Some("main"),
                PushOutcome::Pushed {
                    from: "a".repeat(40),
                    to: "b".repeat(40),
                },
            ),
            target(
                "blog",
                Some("feat"),
                PushOutcome::Held { by: SyncHold::Busy },
            ),
            target("site", Some("main"), PushOutcome::NotAhead),
            target(
                "zap",
                Some("main"),
                PushOutcome::NeedsHuman {
                    reason: BranchNeedsHuman::Diverged,
                },
            ),
            target("gro", Some("topic"), PushOutcome::NoUpstream),
            target("uz", Some("main"), PushOutcome::InSync),
            target("mdz", None, PushOutcome::Detached),
            target("gone", None, PushOutcome::Unread),
            target(
                "tsv",
                Some("main"),
                PushOutcome::PushFailed {
                    failure: RemoteFailure::Unreachable {
                        cause: UnreachableCause::Auth,
                        message: "git@github.com: Permission denied (publickey).".into(),
                    },
                },
            ),
        ],
    );
    let text = render_push_summary(&pushed, VIEW);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "failed        tsv (push: access denied)",
            format!("              hint: {AUTH_HINT}").as_str(),
            "needs human   zap (diverged +1 −2)",
            format!("              hint: {DIVERGED_HINT}").as_str(),
            "pushed        app +2",
            "in sync       uz",
            "held          blog:feat +1 (busy)",
            "not pushed    site (behind 3)  gro:topic (no upstream on origin)  mdz (detached HEAD)",
            "              gone (missing)",
            format!("              hint: {BEHIND_HINT}").as_str(),
            format!("              hint: {NEW_BRANCH_HINT}").as_str(),
            "~/dev/repos.toml · fetched 3h ago",
        ],
        "{text}"
    );
}

/// A branch with no upstream on origin: created under `--new-branch`,
/// found there already, left out by the refspec, or not one
/// `--new-branch` creates — each hint said once.
#[test]
fn new_branches_read_as_what_the_push_did() {
    use fuz_repos::report::{CheckoutPush, PushOutcome};
    let with = |key: &str, b: BranchStatus| {
        let mut e = entry(key, main(), &b.name);
        e.branches = vec![b];
        e
    };
    let untracked =
        |name: &str, upstream| branch(name, upstream, Relation::Untracked, 1, Verdict::LocalOnly);
    let gone = |name: &str, upstream| {
        branch(
            name,
            Some(upstream),
            Relation::Gone,
            1,
            cleanup(CleanupReason::UpstreamGone),
        )
    };
    let r = report(vec![
        with("app", untracked("topic", None)),
        with("blog", untracked("topic", None)),
        with("site", untracked("topic", None)),
        with("zap", untracked("topic", None)),
        with("gro", gone("feat", "origin/feat")),
        with("mdz", untracked("fork", Some("upstream/fork"))),
        with("uz", gone("feat", "origin/old")),
        with(
            "tsv",
            BranchStatus {
                unique_commits: 0,
                ..gone("done", "origin/done")
            },
        ),
    ]);
    let target = |key: &str, branch: &str, outcome| CheckoutPush {
        key: key.into(),
        checkout: format!("/home/me/dev/{key}"),
        branch: Some(branch.to_owned()),
        fetch: FetchOutcome::Fetched,
        outcome,
    };
    let report = PushReport::new(
        r,
        vec![
            target("app", "topic", PushOutcome::Created { to: "c".repeat(40) }),
            target(
                "blog",
                "topic",
                PushOutcome::RemoteBranchExists {
                    at: "0123456789".repeat(4),
                },
            ),
            target(
                "site",
                "topic",
                PushOutcome::NeedsHuman {
                    reason: BranchNeedsHuman::Unmapped,
                },
            ),
            target("zap", "topic", PushOutcome::NoUpstream),
            target("gro", "feat", PushOutcome::NoUpstream),
            target("mdz", "fork", PushOutcome::NoUpstream),
            target("uz", "feat", PushOutcome::NoUpstream),
            target("tsv", "done", PushOutcome::NoUpstream),
        ],
    );
    assert!(!report.in_sync());
    let text = render_push_summary(&report, VIEW);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "needs human   blog:topic (on origin already, at 0123456)  site:topic (outside \
             refspec)",
            format!("              hint: {UNMAPPED_HINT}").as_str(),
            format!("              hint: {EXISTS_HINT}").as_str(),
            "pushed        app:topic (new branch)",
            "not pushed    zap:topic (no upstream on origin)  gro:feat (upstream gone from \
             origin)",
            "              mdz:fork (tracks upstream/fork)  uz:feat (upstream gone from origin)",
            "              tsv:done (nothing unique, upstream gone from origin)",
            format!("              hint: {NEW_BRANCH_HINT}").as_str(),
            format!("              hint: {MERGED_HINT}").as_str(),
            format!("              hint: {OTHER_UPSTREAM_HINT}").as_str(),
            "~/dev/repos.toml · fetched 3h ago",
        ],
        "{text}"
    );
    // created alone: in sync, and no hint
    let created = PushReport::new(report.status.clone(), vec![report.pushes[0].clone()]);
    assert!(created.in_sync());
    assert_eq!(
        render_push_summary(&created, VIEW),
        "pushed        app:topic (new branch)\n~/dev/repos.toml · fetched 3h ago\n"
    );
    // the merged branch alone: its own hint, never --new-branch's
    let merged = PushReport::new(report.status.clone(), vec![report.pushes[7].clone()]);
    assert_eq!(
        render_push_summary(&merged, VIEW)
            .lines()
            .collect::<Vec<_>>(),
        [
            "not pushed    tsv:done (nothing unique, upstream gone from origin)",
            format!("              hint: {MERGED_HINT}").as_str(),
            "~/dev/repos.toml · fetched 3h ago",
        ]
    );
}

/// A clone's verdict in the preview, and its outcome in sync's summary.
#[test]
fn clones_read_as_what_sync_would_do_and_did() {
    let held = |key: &str, by| {
        let mut e = missing(key);
        let recipe = e.clone.take().unwrap().recipe().clone();
        e.clone = Some(CloneVerdict::Held { recipe, by });
        e
    };
    let r = report(vec![
        missing("a"),
        held("b", HeldBy::Busy),
        missing("c"),
        missing("d"),
        missing("e"),
    ]);
    let text = render_summary(&r, VIEW, false);
    assert!(
        text.starts_with(
            "sync would    clone a, c, d, e\nheld          clone b (busy)\nclean 0 · \
             on branches 0 · pinned 0"
        ),
        "{text}"
    );
    let outcome = |key: &str, clone| EntrySync {
        key: key.into(),
        fetch: FetchOutcome::NotFetched,
        clone: Some(clone),
        branches: vec![],
    };
    let synced = SyncReport::new(
        r,
        vec![
            outcome(
                "a",
                CloneOutcome::Cloned {
                    branch: "main".into(),
                    head: "c".repeat(40),
                },
            ),
            outcome("b", CloneOutcome::Held { by: SyncHold::Busy }),
            outcome(
                "c",
                CloneOutcome::Held {
                    by: SyncHold::Changed,
                },
            ),
            outcome(
                "d",
                CloneOutcome::CloneFailed {
                    failure: RemoteFailure::Unreachable {
                        cause: UnreachableCause::Auth,
                        message: "git@github.com: Permission denied (publickey).".into(),
                    },
                },
            ),
            outcome(
                "e",
                CloneOutcome::Failed {
                    message: "cloned into /home/me/dev/e, but its HEAD is detached".into(),
                },
            ),
        ],
    );
    let text = render_sync_summary(&synced, VIEW, false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "failed        d (clone: access denied)",
            "              e (clone: cloned into /home/me/dev/e, but its HEAD is detached)",
            format!("              hint: {AUTH_HINT}").as_str(),
            "synced        clone a",
            "held          clone b (busy), c (changed since read, rerun)",
        ],
        "{text}"
    );
}

/// A missing entry naming another's repo waits for a person: its
/// reason in the summary, the clone held with it, and the way out in
/// its block.
#[test]
fn a_missing_entry_sharing_a_repo_needs_a_person() {
    let mut e = missing("twin");
    let recipe = e.clone.take().unwrap().recipe().clone();
    e.clone = Some(CloneVerdict::Held {
        recipe,
        by: HeldBy::Entry,
    });
    e.needs_human = vec![NeedsHuman::CloneSharesRepo { with: "app".into() }];
    let r = report(vec![entry("app", main(), "main"), e]);
    let text = render_summary(&r, VIEW, false);
    assert!(
        text.starts_with(
            "needs human   twin (same repo as app, not cloned)\nheld          clone twin\n"
        ),
        "{text}"
    );
    let block = render_entry(&r.entries[1], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.contains(
            "  needs     same repo as app, not cloned — sync never makes a second copy of a \
             repo: clone ~/dev/twin by hand, or add it as a worktree of app\n"
        ),
        "{block}"
    );
}

/// A missing entry whose repo an unregistered dir clones: its reason,
/// the clone held, and in its block the two ways out.
#[test]
fn a_missing_entry_cloned_under_another_name_needs_a_person() {
    let mut e = missing("app");
    let recipe = e.clone.take().unwrap().recipe().clone();
    e.clone = Some(CloneVerdict::Held {
        recipe,
        by: HeldBy::Entry,
    });
    e.needs_human = vec![NeedsHuman::ClonedUnregistered {
        dir: "app old".into(),
    }];
    let r = report(vec![e]);
    let text = render_summary(&r, VIEW, false);
    assert!(
        text.starts_with(
            "needs human   app (already cloned as app old, not cloned)\nheld          clone \
             app\n"
        ),
        "{text}"
    );
    let block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.contains(
            "  needs     already cloned as app old, not cloned — sync never makes a second \
             copy of a repo: rename ~/'dev/app old' to ~/dev/app, or set the entry's dir to \
             'app old'\n"
        ),
        "{block}"
    );
}

/// A reference asked for: its refresh in the preview and in sync's
/// summary — refreshed as its fetch went — and a pin's refusal.
#[test]
fn a_refresh_reads_as_what_sync_would_do_and_did() {
    let ff = SyncAction::FastForward { commits: 3 };
    let reference = |key: &str, refresh| EntryStatus {
        kind: EntryKind::Reference,
        writable: false,
        visibility: None,
        ci: false,
        refresh: Some(refresh),
        ..entry(key, Mode::Head, "main")
    };
    let mut lib = reference("lib", RefreshVerdict::Act);
    lib.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 3 },
        0,
        act(ff),
    )];
    // fetched and in sync: said by its refresh alone
    let dom = reference("dom", RefreshVerdict::Act);
    let off = reference("off", RefreshVerdict::Act);
    let mut wpt = EntryStatus {
        kind: EntryKind::Reference,
        visibility: None,
        ci: false,
        refresh: Some(RefreshVerdict::Held { by: HeldBy::Pinned }),
        ..entry("wpt", Mode::PinnedOn("fork"), "fork")
    };
    wpt.branches = vec![branch(
        "fork",
        Some("origin/fork"),
        Relation::Behind { commits: 2 },
        0,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 2 },
            by: HeldBy::Pinned,
        },
    )];
    let r = report(vec![lib, dom, off, wpt]);
    let text = render_summary(&r, VIEW, false);
    assert!(
        text.starts_with(
            "sync would    refresh lib, dom, off · ff lib:main −3\nheld          refresh \
             wpt (pinned)\nclean 0 · on branches 0 · pinned 0"
        ),
        "{text}"
    );
    let block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.starts_with("lib  reference · third-party · leave HEAD · refresh\n"),
        "{block}"
    );
    let block = render_entry(&r.entries[3], Path::new("/home/me/dev"), VIEW);
    assert!(
        block
            .starts_with("wpt  reference · owned · pinned · branch fork · refresh held (pinned)\n"),
        "{block}"
    );
    let failure = RemoteFailure::Failed {
        message: "fatal: transport 'ssh' not allowed".into(),
    };
    let mut r = r;
    r.entries[2].fetch_error = Some(failure.clone());
    let sync = |key: &str, fetch, branches| EntrySync {
        key: key.into(),
        fetch,
        clone: None,
        branches,
    };
    let synced = SyncReport::new(
        r,
        vec![
            sync(
                "lib",
                FetchOutcome::Fetched,
                vec![BranchSync {
                    name: "main".into(),
                    outcome: BranchOutcome::FastForwarded {
                        from: "a".repeat(40),
                        to: "b".repeat(40),
                    },
                    repeats: None,
                }],
            ),
            sync("dom", FetchOutcome::Fetched, vec![]),
            sync("off", FetchOutcome::Failed { failure }, vec![]),
            sync(
                "wpt",
                FetchOutcome::NotFetched,
                vec![BranchSync {
                    name: "fork".into(),
                    outcome: BranchOutcome::Held {
                        action: SyncAction::FastForward { commits: 2 },
                        by: SyncHold::Pinned,
                    },
                    repeats: None,
                }],
            ),
        ],
    );
    let text = render_sync_summary(&synced, VIEW, false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "failed        off (fetch: fatal: transport 'ssh' not allowed)",
            "synced        refresh lib, dom · ff lib:main −3",
            "held          refresh wpt (pinned)",
        ],
        "{text}"
    );
}

#[test]
fn a_clean_workspace_is_one_line() {
    let mut feature = entry("site", main(), "feature");
    feature.branches = vec![branch(
        "feature",
        Some("origin/feature"),
        Relation::InSync,
        0,
        Verdict::Quiet,
    )];
    let pinned = EntryStatus {
        writable: false,
        ..entry("oracle", Mode::Pinned, "x")
    };
    let out = render_summary(
        &report(vec![entry("app", main(), "main"), feature, pinned]),
        VIEW,
        false,
    );
    assert_eq!(
        out,
        "clean 1 · on branches 1 · pinned 1      ~/dev/repos.toml · fetched 3h ago\n"
    );
}

#[test]
fn a_pin_holds_quietly() {
    let held = |commits| Verdict::Held {
        action: SyncAction::FastForward { commits },
        by: HeldBy::Pinned,
    };
    // on the branch it lives on, behind a stale ref, with a stale main
    // beside it and local work
    let mut wpt = EntryStatus {
        kind: EntryKind::Reference,
        visibility: None,
        ci: false,
        ..entry("wpt", Mode::PinnedOn("fork"), "fork")
    };
    wpt.branches = vec![
        branch(
            "fork",
            Some("origin/fork"),
            Relation::Behind { commits: 2 },
            0,
            held(2),
        ),
        branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 57 },
            0,
            held(57),
        ),
    ];
    let r = report(vec![wpt.clone()]);
    assert_eq!(
        render_summary(&r, VIEW, false),
        "clean 0 · on branches 0 · pinned 1      ~/dev/repos.toml\n"
    );
    wpt.branches.push(branch(
        "audit",
        None,
        Relation::Untracked,
        1,
        Verdict::LocalOnly,
    ));
    let r = report(vec![wpt]);
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
local-only    wpt:audit (+1, 2d)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml
"
    );
    let block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.starts_with("wpt  reference · owned · pinned · branch fork\n"),
        "{block}"
    );
    assert!(
        block.contains("  branch    fork   origin/fork  behind 2 · 2d → held ff (pinned)\n"),
        "{block}"
    );
}

#[test]
fn groups_by_what_to_do_next() {
    let mut uz = entry("uz", main(), "main");
    uz.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 13 },
            13,
            act(SyncAction::Push { commits: 13 }),
        ),
        branch(
            "arc",
            Some("origin/arc"),
            Relation::Diverged {
                ahead: 2,
                behind: 5,
            },
            2,
            needs(BranchNeedsHuman::Diverged),
        ),
        branch(
            "old",
            Some("origin/old"),
            Relation::Gone,
            1,
            cleanup(CleanupReason::UpstreamGone),
        ),
        branch(
            "done",
            None,
            Relation::Untracked,
            0,
            cleanup(CleanupReason::Merged),
        ),
        branch("wip", None, Relation::Untracked, 1, Verdict::LocalOnly),
        branch(
            "theirs",
            Some("upstream/main"),
            Relation::Untracked,
            0,
            Verdict::Quiet,
        ),
    ];
    uz.checkouts[0].uncommitted.unstaged = 1;
    uz.stashes = 2;
    let mut zzz = entry("zzz", main(), "main");
    zzz.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 3 },
        0,
        act(SyncAction::FastForward { commits: 3 }),
    )];
    zzz.fetched_at = None;
    let blake3 = missing("blake3");
    let mut old = entry("old", main(), "main");
    old.archived = true;
    old.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Ahead { commits: 1 },
        1,
        needs(BranchNeedsHuman::ArchivedAhead),
    )];
    let mut svelte = entry("svelte", Mode::Pinned, "x");
    svelte.writable = false;
    svelte.checkouts[0].head = Head::Detached {
        commit: "abc".into(),
    };
    svelte.branches = vec![branch(
        "audit",
        None,
        Relation::Untracked,
        4,
        Verdict::LocalOnly,
    )];
    let mut wpt = entry("wpt", Mode::Follow("fork"), "fork");
    wpt.branches = vec![branch(
        "fork",
        Some("origin/fork"),
        Relation::Unmapped,
        3,
        needs(BranchNeedsHuman::Unmapped),
    )];
    wpt.needs_human = vec![NeedsHuman::OperationInProgress {
        checkout: "/home/me/dev/wpt".into(),
        op: InProgressOp::Rebase,
    }];
    wpt.fetch_error = Some(RemoteFailure::RefGone {
        refname: "fork".into(),
        fix: RefGoneFix::ByHand,
    });

    let r = report(vec![
        uz,
        zzz,
        blake3,
        old,
        svelte,
        wpt,
        entry("quiet", main(), "main"),
    ]);
    let out = render_summary(&r, VIEW, false);
    let want = "\
failed        wpt (fetch: origin has no fork)
              hint: a fetch refspec names a branch deleted or renamed on the remote, so nothing was \
             fetched — each entry's repair under --verbose
needs human   uz:arc (diverged +2 −5)  old (archived, +1)  wpt (rebase in progress)
              wpt (outside refspec)
sync would    push uz +13 · ff zzz −3 · clone blake3
local-only    uz:wip (+1, 2d)  svelte:audit (+4, 2d, read-only)
uncommitted   uz (1)
cleanup       uz:old (upstream gone, +1)  uz:done (merged)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago, 1 never
";
    assert_eq!(out, want);

    let verbose = render_summary(&r, VIEW, true);
    assert!(
        verbose.contains("uncommitted   uz (1 unstaged)\n"),
        "{verbose}"
    );
    assert!(verbose.contains("stashes       uz (2)\n"), "{verbose}");
}

#[test]
fn remote_failures_and_the_visibility_check() {
    let unreachable = |cause, message: &str| RemoteFailure::Unreachable {
        cause,
        message: message.into(),
    };
    let with = |key: &str, fetch: Option<RemoteFailure>, check: Option<VisibilityCheck>| {
        let mut e = entry(key, main(), "main");
        e.fetch_error = fetch;
        e.visibility_check = check;
        e
    };
    let private = |key: &str, check| {
        let mut e = with(key, None, Some(check));
        e.visibility = Some(Visibility::Private);
        e
    };
    let r = report(vec![
        with(
            "spec",
            Some(RemoteFailure::RefGone {
                refname: "refs/heads/feat".into(),
                fix: RefGoneFix::UnsetRefspec {
                    pattern: r"^\+?refs/heads/feat(:|$)".into(),
                },
            }),
            None,
        ),
        with(
            "dns",
            Some(unreachable(
                UnreachableCause::Dns,
                "ssh: Could not resolve hostname github.com: Name or service not known",
            )),
            None,
        ),
        with(
            "conn",
            Some(unreachable(
                UnreachableCause::Connection,
                "ssh: connect to host github.com port 22: Connection refused",
            )),
            None,
        ),
        with(
            "key",
            Some(unreachable(
                UnreachableCause::HostKey,
                "Host key verification failed.",
            )),
            None,
        ),
        with(
            "auth",
            Some(unreachable(
                UnreachableCause::Auth,
                "git@github.com: Permission denied (publickey).",
            )),
            None,
        ),
        with(
            "gone",
            Some(RemoteFailure::RepoNotFound {
                message: "ERROR: Repository not found.".into(),
            }),
            None,
        ),
        with(
            "slow",
            Some(RemoteFailure::TimedOut { after_secs: 120 }),
            None,
        ),
        with(
            "tagged",
            Some(RemoteFailure::RefspecOutsideOrigin {
                refspec: "+refs/tags/*:refs/tags/*".into(),
            }),
            None,
        ),
        with(
            "odd",
            Some(RemoteFailure::Failed {
                message: "fatal: the remote end hung up unexpectedly".into(),
            }),
            None,
        ),
        private("leaky", VisibilityCheck::Leak),
        private("sealed", VisibilityCheck::Private),
        private(
            "unsure",
            VisibilityCheck::Unknown {
                failure: RemoteFailure::TimedOut { after_secs: 120 },
            },
        ),
        private(
            "tls",
            VisibilityCheck::Unknown {
                failure: unreachable(
                    UnreachableCause::HostKey,
                    "fatal: unable to access 'https://github.com/me/tls/': server \
                     verification failed: certificate signer not trusted.",
                ),
            },
        ),
    ]);
    let out = render_summary(&r, VIEW, false);
    let want = "\
visibility    leaky (declared private, anonymously readable)
failed        spec (fetch: origin has no refs/heads/feat)  dns (fetch: host not found)
              conn (fetch: no connection)  key (fetch: host not trusted)
              auth (fetch: access denied)  gone (fetch: repo not found)
              slow (fetch: timed out after 120s)
              tagged (fetch: not run — refspec +refs/tags/*:refs/tags/* writes outside \
             refs/remotes/origin/)
              odd (fetch: fatal: the remote end hung up unexpectedly)
              unsure (visibility check: timed out after 120s)
              tls (visibility check: host not trusted)
              hint: a fetch refspec names a branch deleted or renamed on the remote, so nothing was \
             fetched — each entry's repair under --verbose
              hint: repos never asks to trust a host — check its key (or certificate), then connect \
             once by hand to record it
              hint: the host refused this machine's credentials — over SSH, check the host knows the \
             key and a key with a passphrase is loaded in ssh-agent; over HTTPS, check the \
             credential helper holds a valid token
              hint: the host's HTTPS certificate didn't verify — check it, and the system's CA \
             certificates, then rerun repos status --fetch
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
";
    assert_eq!(out, want);

    let block = |key: &str| {
        let e = r.entries.iter().find(|e| e.key == key).unwrap();
        render_entry(e, Path::new("/home/me/dev"), VIEW)
    };
    let spec = block("spec");
    assert!(
        spec.contains(
            "  error     fetch: origin has no refs/heads/feat\n  hint      a fetch refspec \
             names a branch deleted or renamed on the remote, so nothing was fetched — git -C \
             ~/dev/spec config --unset-all remote.origin.fetch '^'\\\\'+?refs/heads/feat(:|$)' \
             drops just that refspec\n"
        ),
        "{spec}"
    );
    assert_eq!(
        ref_gone_hint(&RefGoneFix::SetBranches { branch: None }, "~/dev/spec"),
        "a fetch refspec names a branch deleted or renamed on the remote, so nothing was \
         fetched, and no other refspec in the repo's config would remain — git -C ~/dev/spec \
         remote set-branches origin <branch> points it at a branch the remote has"
    );
    assert!(
        ref_gone_hint(
            &RefGoneFix::SetBranches {
                branch: Some("main".into())
            },
            "x"
        )
        .contains("git -C x remote set-branches origin main points it")
    );
    assert_eq!(
        ref_gone_hint(&RefGoneFix::ByHand, "x"),
        "a fetch refspec names a branch deleted or renamed on the remote, so nothing was \
         fetched; the refspec naming it is outside the repo's own config (an include, \
         worktree or global config), or none names it as git does — remove it by hand"
    );
    let tls = block("tls");
    assert!(
        tls.contains(&format!("  hint      {CERTIFICATE_HINT}\n")),
        "{tls}"
    );
    assert!(!block("unsure").contains("hint"));
    assert_eq!(
        remote_failure_label(
            &RemoteFailure::OriginRefsShared {
                remote: "origin/fork".into(),
                refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
            },
            false
        ),
        "not run — remote origin/fork's refspec +refs/heads/*:refs/remotes/origin/fork/* \
         can write under refs/remotes/origin/, which pruning origin may empty"
    );
    assert_eq!(
        remote_failure_label(
            &RemoteFailure::LegacyRemotesUnreadable {
                path: "/home/me/dev/app/.git/remotes/old".into(),
            },
            false
        ),
        "not run — the legacy remote /home/me/dev/app/.git/remotes/old couldn't be read, \
         and may share origin's refs"
    );
    let dns = block("dns");
    assert!(
        dns.contains(
            "  error     fetch: host not found — ssh: Could not resolve hostname github.com: \
             Name or service not known\n"
        ),
        "{dns}"
    );
    assert!(!dns.contains("hint"), "{dns}");
    assert!(block("key").contains(&format!("  hint      {HOST_KEY_HINT}\n")));
    assert!(block("auth").contains(&format!("  hint      {AUTH_HINT}\n")));
    assert!(
        block("gone")
            .contains("  error     fetch: repo not found — ERROR: Repository not found.\n")
    );
    assert!(block("leaky").contains("  access    anonymously readable, though declared private\n"));
    assert!(block("sealed").contains("  access    private as declared (anonymous read refused)\n"));
    assert!(block("unsure").contains("  error     visibility check: timed out after 120s\n"));

    // loudest: first, and red
    let colored = render_summary(
        &r,
        View {
            color: true,
            ..VIEW
        },
        false,
    );
    assert!(
        colored.starts_with("\x1b[31mvisibility\x1b[0m    leaky"),
        "{colored}"
    );
    // a check that found the repo private says nothing
    let quiet = report(vec![private("sealed", VisibilityCheck::Private)]);
    assert_eq!(
        render_summary(&quiet, VIEW, false),
        "clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago\n"
    );
}

#[test]
fn a_failed_probe_of_a_partial_clone_hints_how_to_fill_it() {
    let failed = |key: &str, partial_filter: Option<&str>| {
        let mut e = entry(key, main(), "main");
        e.checkouts = vec![];
        e.fetched_at = None;
        e.layout = Some(Layout {
            shallow: false,
            sparse: false,
            partial_filter: partial_filter.map(str::to_owned),
        });
        e.probe_error = Some("git status failed (128): error: bad tree object HEAD".into());
        e
    };
    let r = report(vec![failed("app", Some("tree:0")), failed("full", None)]);
    let out = render_summary(&r, VIEW, false);
    assert!(
        out.starts_with(
            "\
failed        app (probe: git status failed (128): error: bad tree object HEAD)
              full (probe: git status failed (128): error: bad tree object HEAD)
              hint: a partial clone may lack objects the probe needs, and repos never fetches \
             them — git -C <dir> checkout fetches them from origin and fills the checkout \
             (each under --verbose)
"
        ),
        "{out}"
    );
    let workspace = Path::new("/home/me/dev");
    let app = render_entry(&r.entries[0], workspace, VIEW);
    assert!(
        app.ends_with(
            "  error     probe: git status failed (128): error: bad tree object HEAD
  hint      a partial clone may lack objects the probe needs, and repos never fetches them — \
             git -C ~/dev/app checkout fetches them from origin and fills the checkout
"
        ),
        "{app}"
    );
    assert!(
        app.contains("  state     never fetched · filter tree:0\n"),
        "{app}"
    );
    let full = render_entry(&r.entries[1], workspace, VIEW);
    assert!(!full.contains("hint"), "{full}");
    // no partial clone failed: no hint
    let r = report(vec![failed("full", None)]);
    assert!(!render_summary(&r, VIEW, false).contains("hint"));
}

#[test]
fn an_am_in_progress_is_named() {
    let mut app = entry("app", main(), "main");
    app.needs_human = vec![NeedsHuman::OperationInProgress {
        checkout: "/home/me/dev/app".into(),
        op: InProgressOp::Am,
    }];
    let out = render_summary(&report(vec![app]), VIEW, false);
    assert!(
        out.starts_with("needs human   app (am in progress)\n"),
        "{out}"
    );
}

#[test]
fn origin_drift_shallow_moves_and_not_a_repo() {
    let mut blog = entry("fuz_blog", main(), "main");
    blog.url = "https://github.com/fuzdev/fuz_blog".into();
    blog.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Ahead { commits: 2 },
        2,
        Verdict::Held {
            action: SyncAction::Push { commits: 2 },
            by: HeldBy::Entry,
        },
    )];
    blog.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Url {
            url: "git@github.com:ryanatkn/fuz_blog".into(),
        },
        expected: "git@github.com:fuzdev/fuz_blog".into(),
        fix: OriginFix::SetUrl,
    }];
    let mut kit = entry("kit", Mode::Head, "main");
    kit.writable = false;
    kit.url = "https://github.com/sveltejs/kit".into();
    kit.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Url {
            url: "https://codeberg.org/someone/kit".into(),
        },
        expected: "https://github.com/sveltejs/kit".into(),
        fix: OriginFix::SetUrl,
    }];
    let mut test262 = entry("test262", Mode::Head, "x");
    test262.checkouts[0].head = Head::Detached {
        commit: "abc".into(),
    };
    test262.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Shallow,
            0,
            act(SyncAction::Move),
        ),
        branch(
            "work",
            Some("origin/work"),
            Relation::Shallow,
            2,
            needs(BranchNeedsHuman::ShallowLocalWork),
        ),
    ];
    let mut goblins = entry("goblins", Mode::Head, "x");
    goblins.presence = Presence::NotARepo;
    goblins.checkouts.clear();
    goblins.layout = None;
    goblins.needs_human = vec![NeedsHuman::NotARepo {
        detail: "empty directory".into(),
    }];

    let r = report(vec![blog, kit, test262, goblins]);
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
needs human   test262:work (shallow, tips differ, +2 local)  goblins (not a repo)
origin drift  fuz_blog (ryanatkn/fuz_blog)  kit (https://codeberg.org/someone/kit)
              hint: git -C <dir> remote set-url origin <url> (each under --verbose)
sync would    move test262:main
held          push fuz_blog +2
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    let blog_block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        blog_block.contains(
            "  needs     origin is git@github.com:ryanatkn/fuz_blog — git -C ~/dev/fuz_blog \
             remote set-url origin git@github.com:fuzdev/fuz_blog\n"
        ),
        "{blog_block}"
    );
    let kit_block = render_entry(&r.entries[1], Path::new("/home/me/dev"), VIEW);
    assert!(
        kit_block.contains(
            "  needs     origin is https://codeberg.org/someone/kit — git -C ~/dev/kit remote \
             set-url origin https://github.com/sveltejs/kit\n"
        ),
        "{kit_block}"
    );
    let mut by_hand = r.entries[0].clone();
    by_hand.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Url {
            url: "https://***@github.com/old/fuz_blog".into(),
        },
        expected: "git@github.com:fuzdev/fuz_blog".into(),
        fix: OriginFix::ByHand {
            reason: OriginByHand::OutsideRepoFile,
        },
    }];
    let block = render_entry(&by_hand, Path::new("/home/me/dev"), VIEW);
    assert!(
        block.contains(
            "  needs     origin is https://***@github.com/old/fuz_blog — set remote.origin.url \
             to git@github.com:fuzdev/fuz_blog by hand: a URL comes from beyond the repo's own \
             config file (global, system, included, or worktree config)\n"
        ),
        "{block}"
    );
    // each reason reads true for its own case
    for (origin, reason, why) in [
        (
            OriginRemote::NoUrl,
            OriginByHand::EmptyValue,
            "origin has no URL — set remote.origin.url to git@github.com:fuzdev/fuz_blog by \
             hand: an empty url among several resets the list, and git remote set-url can't \
             choose among several\n",
        ),
        (
            OriginRemote::NoUrl,
            OriginByHand::ValuelessUrl,
            "origin has no URL — set remote.origin.url to git@github.com:fuzdev/fuz_blog by \
             hand: a url with no value breaks every git remote command\n",
        ),
        (
            OriginRemote::Url {
                url: "git@github.com:old/fuz_blog".into(),
            },
            OriginByHand::SeveralUrls,
            "origin is git@github.com:old/fuz_blog — set remote.origin.url to \
             git@github.com:fuzdev/fuz_blog by hand: it has several URLs, which git remote \
             set-url can't choose among\n",
        ),
    ] {
        let mut e = by_hand.clone();
        e.needs_human = vec![NeedsHuman::OriginMismatch {
            origin,
            expected: "git@github.com:fuzdev/fuz_blog".into(),
            fix: OriginFix::ByHand { reason },
        }];
        let block = render_entry(&e, Path::new("/home/me/dev"), VIEW);
        assert!(block.contains(why), "{block}");
    }
    let summary = render_summary(&report(vec![by_hand, r.entries[1].clone()]), VIEW, false);
    assert!(
        summary.contains(
            "hint: remote.origin.url by hand, or git -C <dir> remote set-url origin <url> \
             (each under --verbose)"
        ),
        "{summary}"
    );
    let goblins_block = render_entry(&r.entries[3], Path::new("/home/me/dev"), VIEW);
    assert!(
        goblins_block.contains("  needs     not a repo: empty directory\n"),
        "{goblins_block}"
    );
}

#[test]
fn dirty_holds_and_a_footer_over_repos_only() {
    let mut gro = entry("gro", main(), "main");
    gro.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 1 },
        0,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 1 },
            by: HeldBy::DirtyCheckout,
        },
    )];
    gro.checkouts[0].uncommitted.unstaged = 2;
    // a dormant owned fork, fetched long ago: not the freshness it reports
    let mut spec = entry("spec", Mode::Head, "x");
    spec.kind = EntryKind::Reference;
    spec.fetched_at = Some(NOW - 90 * 86400);
    assert!(
        render_entry(&gro, Path::new("/home/me/dev"), VIEW)
            .contains("behind 1 · 2d → held ff (dirty)\n"),
        "{}",
        render_entry(&gro, Path::new("/home/me/dev"), VIEW)
    );
    assert_eq!(
        render_summary(&report(vec![gro, spec]), VIEW, false),
        "\
held          ff gro −1 (dirty)
uncommitted   gro (2)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
}

#[test]
fn busy_holds_and_sessions() {
    let s = |pid, cwd: &str| Session::at(pid, 0, cwd.into(), SessionSource::SessionFile);
    let mut app = entry("app", main(), "main");
    app.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Ahead { commits: 1 },
        1,
        Verdict::Held {
            action: SyncAction::Push { commits: 1 },
            by: HeldBy::Busy,
        },
    )];
    app.checkouts[0].busy = vec![
        s(41, "/home/me/dev/app/src"),
        Session {
            worktree: Some("/home/me/dev/app/.claude/worktrees/w".into()),
            process_cwd: Some("/home/me/dev/app/src".into()),
            ..s(42, "/home/me/dev")
        },
    ];
    let mut r = report(vec![app]);
    r.sessions = Sessions::Available {
        unscoped: vec![s(7, "/home/me/dev"), s(8, "/srv/x")],
    };
    // unscoped sessions never print by default: one usually sits at the root
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
held          push app +1 (busy)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    assert_eq!(
        render_summary(&r, VIEW, true),
        "\
held          push app +1 (busy)
unscoped      pid 7 (~/dev)  pid 8 (/srv/x)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    let block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.contains(
            "  checkout  ~/dev/app on main · clean · busy: pid 41 (~/dev/app/src), \
             pid 42 (~/dev, worktree ~/dev/app/.claude/worktrees/w, now ~/dev/app/src)\n"
        ),
        "{block}"
    );
    assert!(block.contains("→ held push (busy)\n"), "{block}");

    // unavailable: said once, first among the failures, and every hold
    // marked
    r.entries[0].checkouts[0].busy.clear();
    r.entries[0].branches[0].verdict = Verdict::Held {
        action: SyncAction::Push { commits: 1 },
        by: HeldBy::BusyUnknown,
    };
    r.entries[0].probe_error = Some("fatal: bad object".into());
    r.sessions = Sessions::Unavailable {
        reason: Unavailable::ForeignPidDomain {
            path: "/home/me/.claude/sessions/9.json".into(),
            pid_domain: "linux:abc:pid:[1]".into(),
            source: SessionSource::SessionFile,
        },
    };
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
failed        busy detection (~/.claude/sessions/9.json is from another machine or pid namespace (linux:abc:pid:[1]) — remove it if that session is gone; every push, ff, and move held)
              app (probe: fatal: bad object)
held          push app +1 (busy unknown)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml
"
    );
    let labels = [
        (
            Unavailable::HomeUnknown,
            "HOME isn't set, so ~/.claude can't be found",
        ),
        (
            Unavailable::Unreadable {
                path: "/proc/self/stat".into(),
                error: "No such file or directory (os error 2)".into(),
            },
            "can't read /proc/self/stat: No such file or directory (os error 2)",
        ),
        (
            Unavailable::Unparseable {
                path: "/home/me/.claude/daemon/roster.json".into(),
                error: "missing field `workers`".into(),
            },
            "can't parse ~/.claude/daemon/roster.json: missing field `workers`",
        ),
        (
            Unavailable::RelativeConfigDir {
                path: "claude".into(),
            },
            "config dir claude isn't an absolute path",
        ),
        (
            Unavailable::ForeignPidDomain {
                path: "/home/me/.claude/daemon/roster.json".into(),
                pid_domain: "linux:abc:pid:[1]".into(),
                source: SessionSource::RosterWorker,
            },
            "~/.claude/daemon/roster.json is from another machine or pid namespace \
             (linux:abc:pid:[1])",
        ),
        // the hint follows what recorded it, wherever the file sits
        (
            Unavailable::ForeignPidDomain {
                path: "/srv/sessions/roster.json".into(),
                pid_domain: "linux:abc:pid:[1]".into(),
                source: SessionSource::RosterWorker,
            },
            "/srv/sessions/roster.json is from another machine or pid namespace \
             (linux:abc:pid:[1])",
        ),
        (
            Unavailable::ForeignPidDomain {
                path: "/srv/9.json".into(),
                pid_domain: "linux:abc:pid:[1]".into(),
                source: SessionSource::SessionFile,
            },
            "/srv/9.json is from another machine or pid namespace \
             (linux:abc:pid:[1]) — remove it if that session is gone",
        ),
    ];
    for (reason, label) in labels {
        assert_eq!(unavailable_label(&reason, VIEW), label);
    }
}

#[test]
fn entry_block() {
    let mut e = entry("gro", main(), "main");
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 1 },
            1,
            act(SyncAction::Push { commits: 1 }),
        ),
        branch("feature-x", None, Relation::Untracked, 0, Verdict::Quiet),
    ];
    e.branches[0].worktree = Some("/home/me/dev/gro".into());
    e.checkouts[0].uncommitted.unstaged = 1;
    e.stashes = 1;
    assert_eq!(
        render_entry(&e, Path::new("/home/me/dev"), VIEW),
        "\
gro  repo · owned · public · ci · follow main
  url       https://github.com/me/gro
  state     fetched 3h ago · stashes 1
  checkout  ~/dev/gro on main · 1 unstaged
  branch    main       origin/main  ahead 1 · 1 unique · 2d · checked out → push
  branch    feature-x  -            untracked · 2d
"
    );
}

#[test]
fn linked_worktrees_in_the_summary() {
    let mut app = entry("app", main(), "main");
    let mut dirty = linked("/home/me/dev/app-feat", "feat");
    dirty.uncommitted.unstaged = 2;
    dirty.uncommitted.untracked = 1;
    app.checkouts.push(dirty);
    app.checkouts.push(linked("/home/me/wt/app-old", "old"));
    app.branches = vec![
        branch(
            "feat",
            Some("origin/feat"),
            Relation::Behind { commits: 1 },
            0,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 1 },
                by: HeldBy::DirtyCheckout,
            },
        ),
        branch(
            "old",
            Some("origin/old"),
            Relation::Gone,
            0,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: Some("/home/me/wt/app-old".into()),
            },
        ),
        branch(
            "usb",
            Some("origin/usb"),
            Relation::Behind { commits: 4 },
            0,
            Verdict::Held {
                action: SyncAction::FastForward { commits: 4 },
                by: HeldBy::UnprobedWorktree,
            },
        ),
    ];
    // two worktrees sharing a dir name stay apart by path
    let mut other_feat = linked("/home/me/wt/app-feat", "feat-2");
    other_feat.uncommitted.staged = 1;
    app.checkouts.push(other_feat);
    let mut usb = unprobed("/media/usb/app", Some("usb"), UnprobedWhy::Missing);
    usb.locked = true;
    let loses = |losses| Some(Prune::Loses { losses });
    app.unprobed_worktrees = vec![
        status(
            unprobed(
                "/home/me/dev/app-broken",
                None,
                UnprobedWhy::Failed {
                    error: "git status failed (128): fatal: not a git repository\nmore".into(),
                },
            ),
            None,
        ),
        status(
            unprobed("/home/me/dev/app-gone", Some("gone"), UnprobedWhy::Prunable),
            Some(Prune::Safe),
        ),
        status(usb, None),
        // removing would lose something: classify said what
        status(
            unprobed("/home/me/dev/app-spike", None, UnprobedWhy::Prunable),
            loses(vec![PruneLoss::DetachedHead]),
        ),
        status(
            UnprobedWorktree {
                in_progress: Some(InProgressOp::Rebase),
                ..unprobed("/home/me/moved-fix", None, UnprobedWhy::Prunable)
            },
            loses(vec![
                PruneLoss::Operation {
                    op: InProgressOp::Rebase,
                },
                PruneLoss::DetachedHead,
            ]),
        ),
        status(
            unprobed(
                "/home/me/dev/app-deleted",
                Some("feat"),
                UnprobedWhy::Prunable,
            ),
            loses(vec![PruneLoss::MissingBranch {
                name: "feat".into(),
            }]),
        ),
        status(
            UnprobedWorktree {
                head: UnprobedHead::Unknown,
                ..unprobed("/home/me/dev/app-garbled", None, UnprobedWhy::Prunable)
            },
            loses(vec![PruneLoss::UnknownHead]),
        ),
        status(
            unprobed("/home/me/dev/app-rel", Some("main"), UnprobedWhy::Prunable),
            loses(vec![PruneLoss::RelativeGitdir {
                git_dir: "/home/me/dev/app/.git/worktrees/k".into(),
            }]),
        ),
        status(
            unprobed("/home/me/dev/app-held", Some("main"), UnprobedWhy::Prunable),
            loses(vec![
                PruneLoss::Submodules,
                PruneLoss::WorktreeRefs,
                PruneLoss::StagedChanges,
            ]),
        ),
        status(
            unprobed("/home/me/dev/app-lost", Some("main"), UnprobedWhy::Prunable),
            loses(vec![PruneLoss::UnmatchedGitDir]),
        ),
        // the scan found it moved: no command
        status(
            unprobed("/home/me/dev/app-b", Some("main"), UnprobedWhy::Prunable),
            Some(Prune::Moved {
                to: vec!["b-moved".into()],
            }),
        ),
        status(
            unprobed("/home/me/dev/app-c", Some("main"), UnprobedWhy::Prunable),
            Some(Prune::Moved {
                to: vec!["c-copy".into(), "c-moved".into()],
            }),
        ),
    ];
    let r = report(vec![app]);
    // the missing worktree says nothing here but holds its branch
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
failed        app (worktree ~/dev/app-broken: git status failed (128): fatal: not a git repository)
held          ff app:feat −1 (dirty), app:usb −4 (unprobed worktree)
uncommitted   app (clean; 2 worktrees, 4 more)
cleanup       app:old (upstream gone, worktree ~/wt/app-old removable)
              app (worktree ~/dev/app-gone gone — if it moved, move it back (or to the workspace root) and rerun repos status, else git -C ~/dev/app worktree remove ~/dev/app-gone)
              app (worktree ~/dev/app-spike gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its detached HEAD)
              app (worktree ~/moved-fix gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its rebase in progress and its detached HEAD)
              app (worktree ~/dev/app-deleted gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its HEAD (branch feat is gone))
              app (worktree ~/dev/app-garbled gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its HEAD)
              app (worktree ~/dev/app-rel gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its index and HEAD if it isn't gone after all (git dir k names its worktree relatively, which git versions resolve differently))
              app (worktree ~/dev/app-held gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards its submodules' repos and its worktree refs and its staged changes)
              app (worktree ~/dev/app-lost gone — if it moved, move it back (or to the workspace root) and rerun repos status; removing discards whatever its git dir holds (it can't be matched))
              app (worktree ~/dev/app-b gone — moved to b-moved; see its line)
              app (worktree ~/dev/app-c gone — moved to c-copy, c-moved; see their lines)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    assert!(
        render_summary(&r, VIEW, true)
            .contains("uncommitted   app (worktree ~/dev/app-feat, 2 unstaged, 1 untracked)")
    );
}

#[test]
fn uncommitted_is_one_item_per_entry() {
    let dirty = |path: &str, head: &str, n: u32| {
        let mut c = linked(path, head);
        c.uncommitted.untracked = n;
        c
    };
    // the primary alone
    let mut solo = entry("solo", main(), "main");
    solo.checkouts[0].uncommitted.unstaged = 1_040;
    // one other worktree stays named, with a clean primary or a dirty one
    let mut one = entry("one", main(), "main");
    one.checkouts
        .push(dirty("/home/me/dev/one-feat", "feat", 2));
    one.checkouts.push(linked("/home/me/dev/one-old", "old"));
    let mut both = entry("both", main(), "main");
    both.checkouts[0].uncommitted.staged = 3;
    both.checkouts
        .push(dirty("/home/me/dev/both-feat", "feat", 5));
    // several fold into a count and their summed dirt
    let mut app = entry("app", main(), "main");
    app.checkouts.push(dirty("/home/me/dev/app-a", "a", 1));
    app.checkouts.push(linked("/home/me/dev/app-b", "b"));
    app.checkouts.push(dirty("/home/me/dev/app-c", "c", 3));
    let mut big = entry("big", main(), "main");
    big.checkouts[0].uncommitted.unstaged = 12;
    for i in 0..1_001 {
        big.checkouts
            .push(dirty(&format!("/home/me/scratch/big-{i}"), "x", 2));
    }
    let r = report(vec![solo, one, both, app, big]);
    let summary = render_summary(&r, VIEW, false);
    assert_eq!(
        summary
            .lines()
            .skip_while(|l| !l.starts_with("uncommitted"))
            .take_while(|l| l.starts_with("uncommitted") || l.starts_with(' '))
            .collect::<Vec<_>>(),
        [
            "uncommitted   solo (1,040)  one (worktree ~/dev/one-feat, 2)",
            "              both (3; worktree ~/dev/both-feat, 5 more)  app (clean; 2 worktrees, 4 more)",
            "              big (12; 1,001 worktrees, 2,002 more)",
        ]
    );
    // `--verbose` keeps each dirty checkout its own item, in detail
    let verbose = render_summary(&r, VIEW, true);
    for item in [
        "solo (1040 unstaged)",
        "one (worktree ~/dev/one-feat, 2 untracked)",
        "both (3 staged)",
        "both (worktree ~/dev/both-feat, 5 untracked)",
        "app (worktree ~/dev/app-a, 1 untracked)",
        "app (worktree ~/dev/app-c, 3 untracked)",
        "big (worktree ~/scratch/big-1000, 2 untracked)",
    ] {
        assert!(verbose.contains(item), "{item} in {verbose}");
    }
    assert!(!verbose.contains("worktrees,"));
}

#[test]
fn digits_group_by_three() {
    for (n, grouped) in [
        (0, "0"),
        (7, "7"),
        (999, "999"),
        (1_000, "1,000"),
        (1_040, "1,040"),
        (12_345, "12,345"),
        (123_456, "123,456"),
        (1_234_567, "1,234,567"),
        (u64::MAX, "18,446,744,073,709,551,615"),
    ] {
        assert_eq!(group_digits(n), grouped);
    }
}

#[test]
fn linked_worktrees_in_the_entry_block() {
    let mut e = entry("app", main(), "main");
    let mut rebasing = linked("/home/me/dev/app-fix", "x");
    rebasing.head = Head::Detached {
        commit: "0123456789abcdef".into(),
    };
    rebasing.in_progress = Some(InProgressOp::Rebase);
    rebasing.uncommitted.conflicted = 1;
    let mut locked = linked("/home/me/wt/app-keep", "keep");
    locked.locked = true;
    e.checkouts.push(linked("/home/me/wt/app-old", "old"));
    e.checkouts.push(rebasing);
    e.checkouts.push(locked);
    let mut main_wt = linked("/home/me/dev/app-main", "trunk");
    main_wt.linked = false;
    e.checkouts.push(main_wt);
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::InSync,
            0,
            Verdict::Quiet,
        ),
        branch(
            "old",
            Some("origin/old"),
            Relation::Gone,
            0,
            Verdict::Cleanup {
                reason: CleanupReason::UpstreamGone,
                removable_worktree: Some("/home/me/wt/app-old".into()),
            },
        ),
    ];
    e.branches[0].worktree = Some("/home/me/dev/app".into());
    e.branches[1].worktree = Some("/home/me/wt/app-old".into());
    let mut usb = unprobed("/media/usb/app", None, UnprobedWhy::Missing);
    usb.locked = true;
    usb.in_progress = Some(InProgressOp::Merge);
    e.unprobed_worktrees = vec![
        status(
            unprobed(
                "/home/me/dev/app-broken",
                Some("broken"),
                UnprobedWhy::Failed {
                    error: "fatal: not a git repository".into(),
                },
            ),
            None,
        ),
        status(
            unprobed("/home/me/dev/app-gone", Some("gone"), UnprobedWhy::Prunable),
            Some(Prune::Safe),
        ),
        status(usb, None),
        status(
            UnprobedWorktree {
                head: UnprobedHead::Unknown,
                ..unprobed(
                    "/home/me/dev/app/.git/worktrees/x",
                    None,
                    UnprobedWhy::Failed {
                        error: "not listed by git: reading …: Permission denied".into(),
                    },
                )
            },
            None,
        ),
    ];
    e.needs_human = vec![
        NeedsHuman::OperationInProgress {
            checkout: "/home/me/dev/app-fix".into(),
            op: InProgressOp::Rebase,
        },
        NeedsHuman::OperationInProgress {
            checkout: "/media/usb/app".into(),
            op: InProgressOp::Merge,
        },
        NeedsHuman::WorktreeUnreadable {
            path: "/home/me/dev/app/.git/worktrees/x".into(),
        },
        NeedsHuman::CheckoutUnresolvable {
            checkout: "/home/me/sealed/app-wt".into(),
            path: "/home/me/sealed/app-wt".into(),
            error: "Permission denied (os error 13)".into(),
        },
        NeedsHuman::CheckoutUnresolvable {
            checkout: "/home/me/loop/app-wt".into(),
            path: "/home/me/loop".into(),
            error: "Too many levels of symbolic links (os error 40)".into(),
        },
        NeedsHuman::CheckoutUnresolvable {
            checkout: "/home/me/dev/app".into(),
            path: "/home/me/dev/app".into(),
            error: "Permission denied (os error 13)".into(),
        },
        NeedsHuman::UnlistedGitDir {
            git_dir: "/home/me/hand/.git".into(),
            head: UnprobedHead::Branch {
                name: "other".into(),
            },
            busy: vec![Session::at(
                41,
                0,
                "/home/me/hand/src".into(),
                SessionSource::SessionFile,
            )],
        },
        NeedsHuman::UnlistedGitDir {
            git_dir: "/home/me/dev/app-new/.git".into(),
            head: UnprobedHead::Unknown,
            busy: vec![Session::at(
                42,
                0,
                "/home/me/dev/app-new".into(),
                SessionSource::RosterWorker,
            )],
        },
    ];
    assert_eq!(
        render_entry(&e, Path::new("/home/me/dev"), VIEW),
        "\
app  repo · owned · public · ci · follow main
  url       https://github.com/me/app
  state     fetched 3h ago
  checkout  ~/dev/app on main · clean
  checkout  ~/wt/app-old (worktree) on old · clean
  checkout  ~/dev/app-fix (worktree) detached at 0123456789ab · 1 conflicted · rebase in progress
  checkout  ~/wt/app-keep (worktree, locked) on keep · clean
  checkout  ~/dev/app-main (main worktree) on trunk · clean
  checkout  ~/dev/app-broken (worktree, probe failed) on broken
  checkout  ~/dev/app-gone (worktree, prunable) on gone
  checkout  /media/usb/app (worktree, missing, locked) detached at 0123456789ab · merge in progress
  checkout  ~/dev/app/.git/worktrees/x (worktree, probe failed) HEAD unreadable
  branch    main  origin/main  in sync · 2d · checked out
  branch    old   origin/old   upstream gone · 2d · checked out → cleanup, worktree removable (ignored files go with it)
  needs     rebase in progress, worktree ~/dev/app-fix
  needs     merge in progress, worktree /media/usb/app
  needs     worktree git dir unreadable: ~/dev/app/.git/worktrees/x
  needs     worktree ~/sealed/app-wt unresolvable: Permission denied (os error 13)
  needs     worktree ~/loop/app-wt unresolvable at ~/loop: Too many levels of symbolic links (os error 40)
  needs     checkout ~/dev/app unresolvable: Permission denied (os error 13)
  needs     unlisted git dir ~/hand/.git on other shares its refs · busy: pid 41 (~/hand/src)
  needs     unlisted git dir ~/dev/app-new/.git HEAD unreadable shares its refs · busy: pid 42 (~/dev/app-new)
  error     worktree ~/dev/app-broken: fatal: not a git repository
  error     worktree ~/dev/app/.git/worktrees/x: not listed by git: reading …: Permission denied
"
    );
}

fn unregistered(
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

/// One of each kind, over each ownership.
fn strays() -> Vec<UnregisteredClone> {
    vec![
        unregistered(
            "app-copy",
            Some("git@github.com:me/app"),
            true,
            UnregisteredKind::SharedGitDir {
                entry: "app".into(),
                with: Some("/home/me/wt/app-feat".into()),
            },
        ),
        unregistered(
            "app-old",
            Some("git@github.com:me/app"),
            true,
            UnregisteredKind::MovedWorktree {
                entry: "app".into(),
                blocked_by: None,
                exit_noise: None,
            },
        ),
        unregistered(
            "lib",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Clone,
        ),
        unregistered(
            "lib-feat",
            Some("https://github.com/them/lib"),
            false,
            UnregisteredKind::Worktree,
        ),
        unregistered(
            "mine",
            Some("git@github.com:me/mine"),
            true,
            UnregisteredKind::Clone,
        ),
        unregistered(
            "site-orphan",
            None,
            false,
            UnregisteredKind::OrphanedWorktree {
                entry: "site".into(),
            },
        ),
    ]
}

#[test]
fn unregistered_in_the_summary() {
    let mut r = report(vec![entry("app", main(), "main")]);
    r.unregistered = Some(strays());
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
unregistered  owned: app-copy (shares app's git dir with ~/wt/app-feat — don't repair),
              app-old (moved worktree of app — git worktree repair), mine
              third-party: lib, lib-feat (worktree)
              no origin: site-orphan (orphaned worktree of site — its git dir is lost)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    // strays aren't entries: a workspace with only strays is otherwise clean
    for none in [None, Some(vec![])] {
        r.unregistered = none;
        assert_eq!(
            render_summary(&r, VIEW, false),
            "clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago\n"
        );
    }
}

/// A clone's temp dir is named as the tool's own, a leftover or a
/// clone still running, with or without an origin.
#[test]
fn an_unfinished_clone_is_the_tools_leftover() {
    let strays = vec![
        unregistered(
            ".app.repos-clone-41-0123456789abcdef",
            Some("git@github.com:me/app"),
            true,
            UnregisteredKind::UnfinishedClone,
        ),
        unregistered(
            ".lib.repos-clone-42-0123456789abcdef",
            None,
            false,
            UnregisteredKind::UnfinishedClone,
        ),
    ];
    let mut r = report(vec![entry("app", main(), "main")]);
    r.unregistered = Some(strays.clone());
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
unregistered  owned: .app.repos-clone-41-0123456789abcdef (a clone repos didn't finish, or one \
still running — remove it once no repos sync is running)
              no origin: .lib.repos-clone-42-0123456789abcdef (a clone repos didn't finish, or \
one still running — remove it once no repos sync is running)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    assert_eq!(
        render_unregistered(&strays[1], &r, VIEW),
        "\
.lib.repos-clone-42-0123456789abcdef  unregistered · no origin · unfinished clone
  dir       ~/dev/.lib.repos-clone-42-0123456789abcdef
  origin    none
  note      a clone repos didn't finish, or one still running, in its temp dir — nothing to keep: \
remove it once no repos sync is running
"
    );
}

#[test]
fn a_shared_git_dir_git_cannot_name() {
    let u = unregistered(
        "app-copy",
        Some("git@github.com:me/app"),
        true,
        UnregisteredKind::SharedGitDir {
            entry: "app".into(),
            with: None,
        },
    );
    let mut r = report(vec![entry("app", main(), "main")]);
    r.unregistered = Some(vec![u.clone()]);
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
unregistered  owned: app-copy (shares app's git dir with a locked or unreadable worktree — don't \
repair)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    assert!(
        render_unregistered(&u, &r, VIEW).contains(
            "  note      a locked or unreadable worktree uses or may use it: this is a copy"
        ),
        "{}",
        render_unregistered(&u, &r, VIEW)
    );
}

#[test]
fn a_moved_worktree_whose_repair_is_blocked() {
    let moved = |dir: &str, block: RepairBlock| {
        unregistered(
            dir,
            Some("git@github.com:me/app"),
            true,
            UnregisteredKind::MovedWorktree {
                entry: "app".into(),
                blocked_by: Some(block),
                exit_noise: None,
            },
        )
    };
    let git_dir = |id: &str| format!("/home/me/dev/app/.git/worktrees/{id}");
    let strays = vec![
        moved(
            "app-feat",
            RepairBlock::ClaimedDir {
                git_dir: git_dir("app-feat"),
            },
        ),
        moved(
            "s-moved",
            RepairBlock::Rewrites {
                path: "/home/me/dev/q".into(),
                git_dir: git_dir("q"),
            },
        ),
        moved(
            "t-moved",
            RepairBlock::RelativeGitdir {
                git_dir: git_dir("k"),
            },
        ),
        moved(
            "u-moved",
            RepairBlock::UnreadableGitdir {
                git_dir: git_dir("u"),
            },
        ),
        moved("v\u{fffd}", RepairBlock::NonUtf8Path),
        moved(
            "w-moved",
            RepairBlock::NulInGitdir {
                git_dir: git_dir("w"),
            },
        ),
    ];
    let mut r = report(vec![entry("app", main(), "main")]);
    r.unregistered = Some(strays.clone());
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
unregistered  owned: app-feat (moved worktree of app — another moved worktree claims this dir; repair that one first once its repair is offered, then rerun),
              s-moved (moved worktree of app — a repair would also rewrite ~/dev/q; fix that first),
              t-moved (moved worktree of app — a relative gitdir in this repo, which git versions resolve differently; fix by hand),
              u-moved (moved worktree of app — a gitdir in this repo can't be read; fix by hand),
              v\u{fffd} (moved worktree of app — its path isn't UTF-8; rename it to a UTF-8 name, then rerun),
              w-moved (moved worktree of app — a NUL in its git dir's gitdir, so a repair may change nothing; its fix under --verbose)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    let blocks: String = strays
        .iter()
        .map(|u| render_unregistered(u, &r, VIEW))
        .collect();
    assert_eq!(
        blocks,
        "\
app-feat  unregistered · owned · moved worktree of app
  dir       ~/dev/app-feat
  origin    git@github.com:me/app
  note      app's worktree git dir app-feat names this dir, so a repair would point this .git there — repair the moved worktree whose .git names it first, once its repair is offered, then rerun repos status
s-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/s-moved
  origin    git@github.com:me/app
  note      git worktree repair would also rewrite ~/dev/q/.git — app's worktree git dir q names it, and its .git is missing or names another — fix that first
t-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/t-moved
  origin    git@github.com:me/app
  note      app's worktree git dir k names its worktree by a relative path, which git 2.48+ resolves against the git dir and older gits against the cwd — what a repair would touch is uncertain, so none is offered; make that gitdir absolute by hand, then rerun repos status
u-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/u-moved
  origin    git@github.com:me/app
  note      app's worktree git dir u has a gitdir that can't be read by this tool (unreadable, or past its size limit, which git may read fine) — what a repair would touch is unknown, so none is offered; trim or fix it by hand, then rerun repos status
v\u{fffd}  unregistered · owned · moved worktree of app
  dir       ~/dev/v\u{fffd}
  origin    git@github.com:me/app
  note      its path isn't UTF-8, so no repair command here can name it exactly — rename it to a UTF-8 name, then rerun repos status
w-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/w-moved
  origin    git@github.com:me/app
  note      app's worktree git dir w holds a NUL in its gitdir — git lists this worktree by what's before the NUL, while a repair here compares that with this .git and may change nothing; the fix writes this .git into that gitdir
  fix       printf '%s\\n' ~/dev/w-moved/.git > ~/dev/app/.git/worktrees/w/gitdir
"
    );
}

#[test]
fn a_swapped_worktree_and_a_repair_git_complains_through() {
    let swapped = unregistered(
        "wa",
        Some("git@github.com:me/app"),
        true,
        UnregisteredKind::MovedWorktree {
            entry: "app".into(),
            blocked_by: Some(RepairBlock::Swapped {
                git_dir: "/home/me/dev/app/.git/worktrees/wa".into(),
                with: "wb".into(),
            }),
            exit_noise: None,
        },
    );
    let noisy = unregistered(
        "s-moved",
        Some("git@github.com:me/app"),
        true,
        UnregisteredKind::MovedWorktree {
            entry: "app".into(),
            blocked_by: None,
            exit_noise: Some("/home/me/y".into()),
        },
    );
    let mut r = report(vec![entry("app", main(), "main")]);
    r.unregistered = Some(vec![noisy.clone(), swapped.clone()]);
    assert_eq!(
        render_summary(&r, VIEW, false),
        "\
unregistered  owned: s-moved (moved worktree of app — git worktree repair),
              wa (moved worktree of app — swapped with wb; move the dirs back)
clean 1 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    assert_eq!(
        render_unregistered(&swapped, &r, VIEW)
            + &render_unregistered(&noisy, &r, VIEW),
        "\
wa  unregistered · owned · moved worktree of app
  dir       ~/dev/wa
  origin    git@github.com:me/app
  note      swapped by hand with wb: app's worktree git dir wa names this dir while wb's .git names it — move the two dirs back; a repair of either would hijack the other
s-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/s-moved
  origin    git@github.com:me/app
  fix       git -C ~/dev/app worktree repair ~/dev/s-moved
  note      git will complain about ~/y and exit 1, leaving it be; this one is repaired all the same
"
    );
}

#[test]
fn unregistered_blocks() {
    let mut app = entry("app", main(), "main");
    app.dir = "app-dir".into();
    let mut r = report(vec![app]);
    r.unregistered = Some(strays());
    let blocks: String = strays()
        .iter()
        .map(|u| render_unregistered(u, &r, VIEW))
        .collect();
    assert_eq!(
        blocks,
        "\
app-copy  unregistered · owned · shares a git dir of app
  dir       ~/dev/app-copy
  origin    git@github.com:me/app
  note      ~/wt/app-feat uses or may use it: this is a copy, or an orphan whose git-dir id git reused — git worktree repair here would take the git dir from there
app-old  unregistered · owned · moved worktree of app
  dir       ~/dev/app-old
  origin    git@github.com:me/app
  fix       git -C ~/dev/app-dir worktree repair ~/dev/app-old
lib  unregistered · third-party · clone
  dir       ~/dev/lib
  origin    https://github.com/them/lib
lib-feat  unregistered · third-party · worktree
  dir       ~/dev/lib-feat
  origin    https://github.com/them/lib
  note      a worktree git doesn't list for any registered repo, a moved worktree whose .git is a link (replace the link with its file, then rerun repos status), or a .git that can't be read
mine  unregistered · owned · clone
  dir       ~/dev/mine
  origin    git@github.com:me/mine
site-orphan  unregistered · no origin · orphaned worktree of site
  dir       ~/dev/site-orphan
  origin    none
  note      its git dir is gone or holds no HEAD — its index and HEAD are lost, and git worktree repair can't reconnect it
"
    );
}

/// What `sh` reads `word` back as, under `HOME=/home/me`.
fn sh_reads(word: &str) -> String {
    shell_reads("sh", &["-c"], word).unwrap()
}

/// What fish reads `word` back as, under `HOME=/home/me`; `None` when
/// fish isn't on `PATH`.
fn fish_reads(word: &str) -> Option<String> {
    shell_reads("fish", &["--no-config", "-c"], word)
}

/// What `shell` (run with `args`) prints for `printf '%s' <word>`, under
/// `HOME=/home/me`; `None` when it isn't installed.
fn shell_reads(shell: &str, args: &[&str], word: &str) -> Option<String> {
    let out = match std::process::Command::new(shell)
        .args(args)
        .arg(format!("printf '%s' {word}"))
        .env_clear()
        .env("HOME", "/home/me")
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        out => out.unwrap(),
    };
    assert!(
        out.status.success(),
        "{shell} failed on {word}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn shell_words() {
    let cases = [
        ("/home/x/dev/app", "/home/x/dev/app"),
        ("git@github.com:me/app", "git@github.com:me/app"),
        ("https://github.com/me/app", "https://github.com/me/app"),
        ("a-b_c.d+e=f,g", "a-b_c.d+e=f,g"),
        // fish expands a bare `%self` to its PID
        ("%self", "'%self'"),
        ("/srv/a%b", "'/srv/a%b'"),
        // fish honors `\\` and `\'` inside single quotes: a `\` goes
        // outside them
        ("/srv/a\\b", r"'/srv/a'\\'b'"),
        ("/srv/a\\\\b", r"'/srv/a'\\''\\'b'"),
        ("/srv/end\\", r"'/srv/end'\\''"),
        ("/srv/it's\\", r"'/srv/it'\''s'\\''"),
        ("/srv/x\\'y", r"'/srv/x'\\''\''y'"),
        ("", "''"),
        ("/srv/my app", "'/srv/my app'"),
        ("/srv/it's", r"'/srv/it'\''s'"),
        ("/srv/$HOME", "'/srv/$HOME'"),
        ("/srv/a*b", "'/srv/a*b'"),
        ("/srv/`x`;y", "'/srv/`x`;y'"),
        ("~/x", "'~/x'"),
        ("/srv/é", "'/srv/é'"),
    ];
    for (raw, quoted) in cases {
        assert_eq!(shell_quote(raw), quoted);
        // and each shell reads it back as it was
        assert_eq!(sh_reads(quoted), raw, "sh: {quoted}");
        if let Some(read) = fish_reads(quoted) {
            assert_eq!(read, raw, "fish: {quoted}");
        }
    }
}

#[test]
fn paths_as_command_words_keep_the_tilde_outside_the_quotes() {
    let cases = [
        ("/home/me/dev/app", "~/dev/app"),
        ("/home/me/my dev/it's", r"~/'my dev/it'\''s'"),
        ("/home/me", "~"),
        ("/home/me/", "~"),
        ("/home/meadow/x y", "'/home/meadow/x y'"),
        ("/srv/x y", "'/srv/x y'"),
    ];
    for (path, word) in cases {
        assert_eq!(VIEW.show_arg(path), word);
        // each shell expands the `~` against the same home
        let back = sh_reads(word);
        assert_eq!(
            back.trim_end_matches('/'),
            path.trim_end_matches('/'),
            "sh: {word}"
        );
        if let Some(back) = fish_reads(word) {
            assert_eq!(
                back.trim_end_matches('/'),
                path.trim_end_matches('/'),
                "fish: {word}"
            );
        }
    }
    let homeless = View { home: None, ..VIEW };
    assert_eq!(homeless.show_arg("/home/me/x y"), "'/home/me/x y'");
}

#[test]
fn printed_commands_are_shell_quoted() {
    let mut app = entry("app", main(), "main");
    app.dir = "my app".into();
    app.checkouts[0].path = "/home/me/dev/my app".into();
    app.layout = Some(Layout {
        shallow: false,
        sparse: false,
        partial_filter: Some("tree:0".into()),
    });
    app.probe_error = Some("bad tree object HEAD".into());
    app.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Missing,
        expected: "file:///srv/it's/app".into(),
        fix: OriginFix::Add,
    }];
    app.unprobed_worktrees = vec![status(
        unprobed("/srv/it's gone", Some("gone"), UnprobedWhy::Prunable),
        Some(Prune::Safe),
    )];
    let stray = unregistered(
        "new $dir",
        Some("git@github.com:me/app"),
        true,
        UnregisteredKind::MovedWorktree {
            entry: "app".into(),
            blocked_by: None,
            exit_noise: None,
        },
    );
    let mut r = report(vec![app]);
    r.unregistered = Some(vec![stray.clone()]);

    let summary = render_summary(&r, VIEW, false);
    // the path read as prose stays as is; the command's words are quoted
    assert!(
        summary.contains(
            r"app (worktree /srv/it's gone gone — if it moved, move it back (or to the workspace root) and rerun repos status, else git -C ~/'dev/my app' worktree remove '/srv/it'\''s gone')"
        ),
        "{summary}"
    );
    let block = render_entry(&r.entries[0], Path::new("/home/me/dev"), VIEW);
    assert!(
        block.contains(
            r"no origin — git -C ~/'dev/my app' remote add origin 'file:///srv/it'\''s/app'"
        ),
        "{block}"
    );
    assert!(
        block.contains("git -C ~/'dev/my app' checkout fetches them"),
        "{block}"
    );
    assert!(
        render_unregistered(&stray, &r, VIEW)
            .contains("  fix       git -C ~/'dev/my app' worktree repair ~/'dev/new $dir'\n"),
        "{}",
        render_unregistered(&stray, &r, VIEW)
    );
}

#[test]
fn widths_from_columns() {
    assert_eq!(summary_width(None), DEFAULT_WIDTH);
    assert_eq!(summary_width(Some("80")), 80);
    assert_eq!(summary_width(Some(" 120\n")), 120);
    assert_eq!(summary_width(Some("40")), 40);
    for unusable in ["39", "0", "", "wide", "-80", "80.5"] {
        assert_eq!(summary_width(Some(unusable)), DEFAULT_WIDTH, "{unusable}");
    }
}

#[test]
fn color_only_on_a_terminal_without_no_color() {
    assert!(use_color(true, None));
    // no-color.org: an empty value is as good as unset
    assert!(use_color(true, Some(OsStr::new(""))));
    assert!(!use_color(true, Some(OsStr::new("1"))));
    assert!(!use_color(true, Some(OsStr::new("0"))));
    assert!(!use_color(false, None));
    assert!(!use_color(false, Some(OsStr::new(""))));
}

#[test]
fn an_unreadable_git_dir_is_said_once_as_needing_a_person() {
    let mut app = entry("app", main(), "main");
    let failed = |path: &str| {
        status(
            UnprobedWorktree {
                head: UnprobedHead::Unknown,
                ..unprobed(
                    path,
                    None,
                    UnprobedWhy::Failed {
                        error: "not listed by git: reading …: Permission denied".into(),
                    },
                )
            },
            None,
        )
    };
    // the admin dir unreadable itself, one under an unreadable
    // `worktrees/`, and one that failed for its own reason
    app.unprobed_worktrees = vec![
        failed("/home/me/dev/app/.git/worktrees/x"),
        failed("/home/me/dev/lib/.git/worktrees/y"),
        failed("/home/me/dev/app-broken"),
    ];
    app.needs_human = vec![
        NeedsHuman::WorktreeUnreadable {
            path: "/home/me/dev/app/.git/worktrees/x".into(),
        },
        NeedsHuman::WorktreeUnreadable {
            path: "/home/me/dev/lib/.git/worktrees".into(),
        },
        NeedsHuman::DefaultBranchGone {
            branch: "main".into(),
        },
    ];
    assert_eq!(
        render_summary(&report(vec![app]), VIEW, false),
        "\
failed        app (worktree ~/dev/app-broken: not listed by git: reading …: Permission denied)
needs human   app (worktree git dir unreadable: ~/dev/app/.git/worktrees/x)
              app (worktree git dir unreadable: ~/dev/lib/.git/worktrees)
              app (main's upstream is gone from origin)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
}

#[test]
fn color_marks_group_labels_only() {
    let mut app = entry("app", main(), "main");
    app.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Ahead { commits: 1 },
        1,
        act(SyncAction::Push { commits: 1 }),
    )];
    app.needs_human = vec![NeedsHuman::DefaultBranchNoUpstream {
        branch: "main".into(),
    }];
    app.checkouts[0].uncommitted.untracked = 1;
    let mut gro = entry("gro", main(), "main");
    gro.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 2 },
        0,
        Verdict::Held {
            action: SyncAction::FastForward { commits: 2 },
            by: HeldBy::Entry,
        },
    )];
    gro.needs_human = vec![NeedsHuman::OriginMismatch {
        origin: OriginRemote::Missing,
        expected: "git@github.com:me/gro".into(),
        fix: OriginFix::Add,
    }];
    gro.fetch_error = Some(RemoteFailure::Failed {
        message: "fatal: unreachable".into(),
    });
    let r = report(vec![app, gro]);
    let colored = render_summary(
        &r,
        View {
            color: true,
            ..VIEW
        },
        false,
    );
    assert_eq!(
        colored,
        "\
\x1b[31mfailed\x1b[0m        gro (fetch: fatal: unreachable)
\x1b[31mneeds human\x1b[0m   app (main has no origin upstream)
\x1b[33morigin drift\x1b[0m  gro (no origin)
              hint: git -C <dir> remote add origin <url> (each under --verbose)
\x1b[32msync would\x1b[0m    push app +1
\x1b[33mheld\x1b[0m          ff gro −2
uncommitted   app (1)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
    );
    // stripped of its escapes, it's the plain summary
    assert_eq!(
        colored
            .replace("\x1b[31m", "")
            .replace("\x1b[32m", "")
            .replace("\x1b[33m", "")
            .replace("\x1b[0m", ""),
        render_summary(&r, VIEW, false)
    );
    assert!(!render_summary(&r, VIEW, false).contains('\x1b'));
}

#[test]
fn singles_wrap_between_items_with_a_hanging_indent() {
    let items = |items: &[&str]| Items::Singles(items.iter().map(|&i| i.to_owned()).collect());
    let narrow = View { width: 40, ..VIEW };
    let long = "an item far too long to share any line with others";
    assert_eq!(
        render_group(
            "needs human",
            Tone::Red,
            &items(&["aaaa (one)", "bbbb (two)", "cccc (three)", long, "dd"]),
            narrow,
        ),
        "\
needs human   aaaa (one)  bbbb (two)
              cccc (three)
              an item far too long to share any line with others
              dd
"
    );
    // an item ending exactly at the width fits
    assert_eq!(
        render_group(
            "x",
            Tone::Plain,
            &items(&["aaaa (one)", "bbbb (two)"]),
            View { width: 36, ..VIEW },
        ),
        "x             aaaa (one)  bbbb (two)\n"
    );
    assert_eq!(
        render_group(
            "x",
            Tone::Plain,
            &items(&["aaaa (one)", "bbbb (two)"]),
            View { width: 35, ..VIEW },
        ),
        "x             aaaa (one)\n              bbbb (two)\n"
    );
    // widths count chars, not bytes: `−` and `—` are one column each
    assert_eq!(
        render_group(
            "x",
            Tone::Plain,
            &items(&["a −1 —", "b −2 —"]),
            View { width: 28, ..VIEW }
        ),
        "x             a −1 —  b −2 —\n"
    );
    assert_eq!(render_group("x", Tone::Red, &items(&[]), narrow), "");
}

#[test]
fn runs_wrap_one_to_a_line() {
    let run = |items: &[&str]| items.iter().map(|&i| i.to_owned()).collect::<Vec<_>>();
    let runs = Items::Runs(
        vec![
            run(&["push a +1", "bb +2", "ccc +3", "dddd +4"]),
            run(&["ff e −1"]),
            run(&["move f"]),
            run(&["clone g", "h"]),
        ],
        " · ",
    );
    // one line when it fits
    assert_eq!(
        render_group("sync would", Tone::Green, &runs, VIEW),
        "sync would    push a +1, bb +2, ccc +3, dddd +4 · ff e −1 · move f · clone g, h\n"
    );
    // else each run starts a line, its items flowing with the `,` kept
    // at the break, the ` · ` dropped
    assert_eq!(
        render_group("sync would", Tone::Green, &runs, View { width: 40, ..VIEW }),
        "\
sync would    push a +1, bb +2, ccc +3,
              dddd +4
              ff e −1
              move f
              clone g, h
"
    );
}

#[test]
fn a_wrapped_sync_line_in_the_summary() {
    let mut entries = Vec::new();
    for (key, commits) in [("archives", 13), ("setup", 2), ("fuz_util", 1)] {
        let mut e = entry(key, main(), "main");
        e.branches = vec![branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits },
            commits,
            act(SyncAction::Push { commits }),
        )];
        entries.push(e);
    }
    let mut zzz = entry("zzz", main(), "main");
    zzz.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 3 },
        0,
        act(SyncAction::FastForward { commits: 3 }),
    )];
    entries.push(zzz);
    for key in ["blake3", "corpora"] {
        entries.push(missing(key));
    }
    let r = report(entries);
    assert!(render_summary(&r, VIEW, false).starts_with(
        "sync would    push archives +13, setup +2, fuz_util +1 · ff zzz −3 · clone blake3, \
             corpora\n"
    ));
    assert!(
        render_summary(&r, View { width: 50, ..VIEW }, false).starts_with(
            "\
sync would    push archives +13, setup +2,
              fuz_util +1
              ff zzz −3
              clone blake3, corpora
"
        )
    );
}

/// `--brief`'s line: what it selects, in its order, and its words.
#[test]
fn brief_says_sessions_operation_behind_then_ahead() {
    let brief = |e: &EntryStatus| render_brief(e, &e.checkouts[0], VIEW);
    let with = |relation| {
        let mut e = entry("app", main(), "main");
        e.branches = vec![branch(
            "main",
            Some("origin/main"),
            relation,
            0,
            Verdict::Quiet,
        )];
        e
    };
    let session = |pid| {
        Session::at(
            pid,
            0,
            "/home/me/dev/app".into(),
            SessionSource::SessionFile,
        )
    };

    // nothing to say: in sync and idle, and every relation it passes over
    for relation in [
        Relation::InSync,
        Relation::Shallow,
        Relation::Gone,
        Relation::Unmapped,
        Relation::Untracked,
    ] {
        assert_eq!(brief(&with(relation)), None, "{relation:?}");
    }
    // dirt is the session's to see
    let mut dirty = with(Relation::InSync);
    dirty.checkouts[0].uncommitted.unstaged = 3;
    assert_eq!(brief(&dirty), None);

    assert_eq!(
        brief(&with(Relation::Behind { commits: 3 })).as_deref(),
        Some("repos: app — 3 behind origin/main (fetched 3h ago)\n")
    );
    assert_eq!(
        brief(&with(Relation::Ahead { commits: 2 })).as_deref(),
        Some("repos: app — 2 ahead of origin/main (unpushed)\n")
    );
    assert_eq!(
        brief(&with(Relation::Diverged {
            ahead: 1,
            behind: 4
        }))
        .as_deref(),
        Some("repos: app — diverged from origin/main +1 −4 (fetched 3h ago)\n")
    );
    // no fetch time known: the age is left out, never guessed
    let mut unfetched = with(Relation::Behind { commits: 3 });
    unfetched.fetched_at = None;
    assert_eq!(
        brief(&unfetched).as_deref(),
        Some("repos: app — 3 behind origin/main\n")
    );

    // every signal, in order, on one line however narrow the view
    let mut all = with(Relation::Diverged {
        ahead: 2,
        behind: 1,
    });
    // busy for a session elsewhere in the repo alone (an agent
    // worktree's): it works somewhere else, so nothing's said of it
    all.checkouts[0].busy = vec![session(9)];
    all.checkouts[0].in_progress = Some(InProgressOp::Rebase);
    assert_eq!(
        brief(&all).as_deref(),
        Some("repos: app — rebase in progress; diverged from origin/main +2 −1 (fetched 3h ago)\n")
    );
    all.checkouts[0].busy.push(session(10));
    all.checkouts[0].working = vec![session(10)];
    let line = render_brief(&all, &all.checkouts[0], View { width: 40, ..VIEW }).unwrap();
    assert_eq!(
        line,
        "repos: app — another live session is working in this checkout; rebase in \
         progress; diverged from origin/main +2 −1 (fetched 3h ago)\n"
    );
    all.checkouts[0].busy.push(session(11));
    all.checkouts[0].working.push(session(11));
    assert!(
        brief(&all)
            .unwrap()
            .starts_with("repos: app — 2 other live sessions are working in this checkout; "),
    );

    // a failed probe says nothing, whatever it read
    let mut failed = all.clone();
    failed.probe_error = Some("fatal: bad object".into());
    assert_eq!(brief(&failed), None);
}

/// `--brief` on a checkout other than the primary, a detached HEAD, a
/// reference, and a pin.
#[test]
fn brief_reads_its_own_checkout_and_compares_only_owned_branches() {
    let mut e = entry("app", main(), "main");
    e.checkouts.push(linked("/home/me/dev/app-wt", "feat"));
    e.branches = vec![
        branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 5 },
            0,
            Verdict::Quiet,
        ),
        branch(
            "feat",
            Some("origin/feat"),
            Relation::Ahead { commits: 1 },
            1,
            act(SyncAction::Push { commits: 1 }),
        ),
    ];
    e.checkouts[1].in_progress = Some(InProgressOp::CherryPick);
    // the worktree's own branch and operation, never the primary's
    assert_eq!(
        render_brief(&e, &e.checkouts[1], VIEW).as_deref(),
        Some("repos: app — cherry-pick in progress; 1 ahead of origin/feat (unpushed)\n")
    );
    assert_eq!(
        render_brief(&e, &e.checkouts[0], VIEW).as_deref(),
        Some("repos: app — 5 behind origin/main (fetched 3h ago)\n")
    );
    // detached: no branch to compare
    e.checkouts[0].head = Head::Detached {
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
    };
    assert_eq!(render_brief(&e, &e.checkouts[0], VIEW), None);

    // a third-party reference, and a pin, owned or not: sessions and
    // operations only
    let session = Session::at(10, 0, "/home/me/dev/app".into(), SessionSource::SessionFile);
    for (writable, mode) in [
        (false, main()),
        (false, Mode::PinnedOn("main")),
        (true, Mode::PinnedOn("main")),
    ] {
        let mut e = entry("lib", mode, "main");
        e.kind = EntryKind::Reference;
        e.writable = writable;
        e.branches = vec![branch(
            "main",
            Some("origin/main"),
            Relation::Diverged {
                ahead: 1,
                behind: 1,
            },
            1,
            Verdict::LocalOnly,
        )];
        assert_eq!(render_brief(&e, &e.checkouts[0], VIEW), None, "{mode:?}");
        e.checkouts[0].busy = vec![session.clone()];
        e.checkouts[0].working = vec![session.clone()];
        e.checkouts[0].in_progress = Some(InProgressOp::Merge);
        assert_eq!(
            render_brief(&e, &e.checkouts[0], VIEW).as_deref(),
            Some(
                "repos: lib — another live session is working in this checkout; merge in \
                 progress\n"
            ),
            "{mode:?}"
        );
    }
    // an owned reference that isn't pinned is fetched as a repo is
    let mut fork = entry("fork", main(), "main");
    fork.kind = EntryKind::Reference;
    fork.branches = vec![branch(
        "main",
        Some("origin/main"),
        Relation::Behind { commits: 2 },
        0,
        Verdict::Quiet,
    )];
    assert_eq!(
        render_brief(&fork, &fork.checkouts[0], VIEW).as_deref(),
        Some("repos: fork — 2 behind origin/main (fetched 3h ago)\n")
    );
}

#[test]
fn ages() {
    assert_eq!(format_age(5), "5s");
    assert_eq!(format_age(120), "2m");
    assert_eq!(format_age(3 * 3600 + 5), "3h");
    assert_eq!(format_age(3 * 86400), "3d");
    assert_eq!(format_age(90 * 86400), "3mo");
    assert_eq!(format_age(800 * 86400), "2y");
}

#[test]
fn home_paths() {
    assert_eq!(VIEW.show("/home/me/dev"), "~/dev");
    assert_eq!(VIEW.show("/home/me"), "~");
    assert_eq!(VIEW.show("/home/meadow/x"), "/home/meadow/x");
    let homeless = View { home: None, ..VIEW };
    assert_eq!(homeless.show("/x"), "/x");
}
