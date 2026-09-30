//! The every-variant coverage floors: each closed enum a document carries
//! appears in its goldens at least once, in every place it can.
//!
//! Each enum's floor index is its variant's position in one list of
//! patterns (`floor_index!`), which an exhaustive `match` checks covers
//! every variant, and its count is that list's length. So a new variant
//! fails to compile until it's listed, and once listed, the floor fails
//! until a golden covers it — or, for a variant a place can never carry,
//! until it's named among that place's exclusions, beside the rule that
//! says why.

use std::collections::{BTreeMap, BTreeSet};

use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::error::ErrorKind;
use fuz_repos::registry::{CheckoutList, EntryKind, RegistryIssue, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    BranchOutcome, CloneOutcome, ErrorReport, FetchOutcome, PushOutcome, PushReport, RepairBlock,
    Sessions, StatusReport, SyncHold, SyncReport, UnregisteredKind,
};
use fuz_repos::sessions::{Session, SessionSource, Unavailable};
use fuz_repos::state::{
    BranchNeedsHuman, CleanupReason, CloneVerdict, Head, HeldBy, InProgressOp, Presence, Prune,
    PruneLoss, RefreshVerdict, Relation, SyncAction, UnprobedHead, UnprobedWhy, Verdict,
};

/// Defines `$f`, a value's floor index — the position of the first of the
/// patterns it matches — and `$n`, the number of patterns. The patterns
/// must cover every variant, one each: the `match` is exhaustive, and a
/// pattern listed twice is unreachable: a warning (an error under clippy's
/// `-D warnings`), and the floor then fails on the index no value reaches.
macro_rules! floor_index {
    ($f:ident, $n:ident, $t:ty, [$($p:pat),+ $(,)?]) => {
        fn $f(v: &$t) -> usize {
            match v {
                $($p)|+ => {}
            }
            let arms: &[fn(&$t) -> bool] = &[$(|v| matches!(v, $p)),+];
            arms.iter().position(|arm| arm(v)).unwrap()
        }
        const $n: usize = [$(stringify!($p)),+].len();
    };
}

/// The variants seen, by enum (and the place it's carried in, where one
/// enum's places differ in what they can carry), as their floor indices.
#[derive(Default)]
struct Seen(BTreeMap<&'static str, BTreeSet<usize>>);

impl Seen {
    fn mark(&mut self, name: &'static str, id: usize) {
        self.0.entry(name).or_default().insert(id);
    }

    /// Asserts `name` was seen as every index below `n` but `never` — the
    /// variants its place can't carry — and as none of those.
    fn floor(&self, name: &str, n: usize, never: &[usize]) {
        let seen = self.0.get(name).cloned().unwrap_or_default();
        let expected: BTreeSet<usize> = (0..n).filter(|i| !never.contains(i)).collect();
        let missing: Vec<&usize> = expected.difference(&seen).collect();
        let unexpected: Vec<&usize> = seen.difference(&expected).collect();
        assert!(
            missing.is_empty() && unexpected.is_empty(),
            "{name}: no golden covers {missing:?}, and {unexpected:?} can't appear (indices in \
             its floor's list)"
        );
    }
}

// --- the status report's enums ---

floor_index!(
    presence,
    PRESENCE,
    Presence,
    [Presence::Present, Presence::Missing, Presence::NotARepo]
);
floor_index!(
    entry_kind,
    ENTRY_KIND,
    EntryKind,
    [EntryKind::Repo, EntryKind::Reference]
);
floor_index!(
    visibility,
    VISIBILITY,
    Visibility,
    [Visibility::Public, Visibility::Private]
);
floor_index!(
    head,
    HEAD,
    Head,
    [Head::Branch { .. }, Head::Detached { .. }]
);
floor_index!(
    unprobed_head,
    UNPROBED_HEAD,
    UnprobedHead,
    [
        UnprobedHead::Branch { .. },
        UnprobedHead::Detached { .. },
        UnprobedHead::Unknown,
    ]
);
floor_index!(
    unprobed_why,
    UNPROBED_WHY,
    UnprobedWhy,
    [
        UnprobedWhy::Prunable,
        UnprobedWhy::Missing,
        UnprobedWhy::Failed { .. },
    ]
);
floor_index!(
    prune,
    PRUNE,
    Prune,
    [Prune::Safe, Prune::Loses { .. }, Prune::Moved { .. }]
);
floor_index!(
    prune_loss,
    PRUNE_LOSS,
    PruneLoss,
    [
        PruneLoss::Operation { .. },
        PruneLoss::DetachedHead,
        PruneLoss::UnknownHead,
        PruneLoss::MissingBranch { .. },
        PruneLoss::Submodules,
        PruneLoss::WorktreeRefs,
        PruneLoss::StagedChanges,
        PruneLoss::UnmatchedGitDir,
        PruneLoss::RelativeGitdir { .. },
    ]
);
floor_index!(
    in_progress,
    IN_PROGRESS,
    InProgressOp,
    [
        InProgressOp::Rebase,
        InProgressOp::Merge,
        InProgressOp::CherryPick,
        InProgressOp::Revert,
        InProgressOp::Bisect,
        InProgressOp::Sequencer,
        InProgressOp::Am,
    ]
);
floor_index!(
    relation,
    RELATION,
    Relation,
    [
        Relation::InSync,
        Relation::Ahead { .. },
        Relation::Behind { .. },
        Relation::Diverged { .. },
        Relation::Shallow,
        Relation::Gone,
        Relation::Unmapped,
        Relation::Untracked,
    ]
);
floor_index!(
    verdict,
    VERDICT,
    Verdict,
    [
        Verdict::Quiet,
        Verdict::Act { .. },
        Verdict::Held { .. },
        Verdict::NeedsHuman { .. },
        Verdict::LocalOnly,
        Verdict::Cleanup { .. },
    ]
);
floor_index!(
    sync_action,
    SYNC_ACTION,
    SyncAction,
    [
        SyncAction::Push { .. },
        SyncAction::FastForward { .. },
        SyncAction::Move,
    ]
);
floor_index!(
    held_by,
    HELD_BY,
    HeldBy,
    [
        HeldBy::Pinned,
        HeldBy::Entry,
        HeldBy::PushUrl,
        HeldBy::OriginNotHttps,
        HeldBy::FetchFailed,
        HeldBy::DirtyCheckout,
        HeldBy::UnprobedWorktree,
        HeldBy::SeveralCheckouts,
        HeldBy::Busy,
        HeldBy::BusyUnknown,
    ]
);
floor_index!(
    branch_needs_human,
    BRANCH_NEEDS_HUMAN,
    BranchNeedsHuman,
    [
        BranchNeedsHuman::Diverged,
        BranchNeedsHuman::Unmapped,
        BranchNeedsHuman::ArchivedAhead,
        BranchNeedsHuman::ShallowLocalWork,
        BranchNeedsHuman::UpstreamNotABranch,
    ]
);
floor_index!(
    cleanup_reason,
    CLEANUP_REASON,
    CleanupReason,
    [CleanupReason::Merged, CleanupReason::UpstreamGone]
);
floor_index!(
    clone_verdict,
    CLONE_VERDICT,
    CloneVerdict,
    [CloneVerdict::Act { .. }, CloneVerdict::Held { .. }]
);
floor_index!(
    refresh_verdict,
    REFRESH_VERDICT,
    RefreshVerdict,
    [RefreshVerdict::Act, RefreshVerdict::Held { .. }]
);
floor_index!(
    needs_human,
    NEEDS_HUMAN,
    NeedsHuman,
    [
        NeedsHuman::NotARepo { .. },
        NeedsHuman::OperationInProgress { .. },
        NeedsHuman::OriginMismatch { .. },
        NeedsHuman::OriginNotHttps { .. },
        NeedsHuman::FetchUrlMismatch { .. },
        NeedsHuman::WorktreeUnreadable { .. },
        NeedsHuman::DefaultBranchMissing { .. },
        NeedsHuman::DefaultBranchNoUpstream { .. },
        NeedsHuman::DefaultBranchGone { .. },
        NeedsHuman::UnexpectedDetached { .. },
        NeedsHuman::CheckoutUnresolvable { .. },
        NeedsHuman::UnlistedGitDir { .. },
        NeedsHuman::PushUrlMismatch { .. },
        NeedsHuman::CloneSharesRepo { .. },
        NeedsHuman::ClonedUnregistered { .. },
    ]
);
floor_index!(
    origin_remote,
    ORIGIN_REMOTE,
    OriginRemote,
    [
        OriginRemote::Url { .. },
        OriginRemote::NoUrl,
        OriginRemote::Missing,
    ]
);
floor_index!(
    origin_fix,
    ORIGIN_FIX,
    OriginFix,
    [OriginFix::Add, OriginFix::SetUrl, OriginFix::ByHand { .. }]
);
floor_index!(
    origin_by_hand,
    ORIGIN_BY_HAND,
    OriginByHand,
    [
        OriginByHand::OutsideRepoFile,
        OriginByHand::ValuelessUrl,
        OriginByHand::EmptyValue,
        OriginByHand::SeveralUrls,
    ]
);
floor_index!(
    remote_failure,
    REMOTE_FAILURE,
    RemoteFailure,
    [
        RemoteFailure::RefGone { .. },
        RemoteFailure::Unreachable { .. },
        RemoteFailure::RepoNotFound { .. },
        RemoteFailure::TimedOut { .. },
        RemoteFailure::Failed { .. },
        RemoteFailure::Rejected { .. },
        RemoteFailure::RefspecOutsideOrigin { .. },
        RemoteFailure::OriginRefsShared { .. },
        RemoteFailure::LegacyRemotesUnreadable { .. },
    ]
);
floor_index!(
    ref_gone_fix,
    REF_GONE_FIX,
    RefGoneFix,
    [
        RefGoneFix::UnsetRefspec { .. },
        RefGoneFix::SetBranches { .. },
        RefGoneFix::ByHand,
    ]
);
floor_index!(
    unreachable_cause,
    UNREACHABLE_CAUSE,
    UnreachableCause,
    [
        UnreachableCause::Dns,
        UnreachableCause::Connection,
        UnreachableCause::HostKey,
        UnreachableCause::Auth,
    ]
);
floor_index!(
    visibility_check,
    VISIBILITY_CHECK,
    VisibilityCheck,
    [
        VisibilityCheck::Leak,
        VisibilityCheck::Private,
        VisibilityCheck::Unknown { .. },
    ]
);
floor_index!(
    sessions,
    SESSIONS,
    Sessions,
    [Sessions::Available { .. }, Sessions::Unavailable { .. }]
);
floor_index!(
    unavailable,
    UNAVAILABLE,
    Unavailable,
    [
        Unavailable::HomeUnknown,
        Unavailable::RelativeConfigDir { .. },
        Unavailable::Unreadable { .. },
        Unavailable::Unparseable { .. },
        Unavailable::ForeignPidDomain { .. },
    ]
);
floor_index!(
    session_source,
    SESSION_SOURCE,
    SessionSource,
    [SessionSource::SessionFile, SessionSource::RosterWorker]
);
floor_index!(
    unregistered_kind,
    UNREGISTERED_KIND,
    UnregisteredKind,
    [
        UnregisteredKind::Clone,
        UnregisteredKind::Worktree,
        UnregisteredKind::MovedWorktree { .. },
        UnregisteredKind::OrphanedWorktree { .. },
        UnregisteredKind::SharedGitDir { .. },
        UnregisteredKind::UnfinishedClone,
    ]
);
floor_index!(
    repair_block,
    REPAIR_BLOCK,
    RepairBlock,
    [
        RepairBlock::Rewrites { .. },
        RepairBlock::ClaimedDir { .. },
        RepairBlock::Swapped { .. },
        RepairBlock::RelativeGitdir { .. },
        RepairBlock::UnreadableGitdir { .. },
        RepairBlock::NonUtf8Path,
        RepairBlock::NulInGitdir { .. },
    ]
);
floor_index!(
    error_kind,
    ERROR_KIND,
    ErrorKind,
    [
        ErrorKind::MissingCommand,
        ErrorKind::ReferencesWithTargets,
        ErrorKind::RootNotFound,
        ErrorKind::RegistryNotFound,
        ErrorKind::RootInEntry { .. },
        ErrorKind::RegistryRead,
        ErrorKind::RegistryParse,
        ErrorKind::RegistryInvalid { .. },
        ErrorKind::GitNotFound,
        ErrorKind::GitTooOld { .. },
        ErrorKind::UnknownEntry { .. },
        ErrorKind::NoCheckout,
        ErrorKind::PushThirdParty { .. },
        ErrorKind::PushPinned { .. },
        ErrorKind::NewBranchByAgent,
        ErrorKind::Io,
    ]
);
floor_index!(
    registry_issue,
    REGISTRY_ISSUE,
    RegistryIssue,
    [
        RegistryIssue::RepoNotOwned { .. },
        RegistryIssue::ForkNotOwned { .. },
        RegistryIssue::DirNotAName { .. },
        RegistryIssue::DirClaimedTwice { .. },
        RegistryIssue::KeyInBoth { .. },
        RegistryIssue::KeyIsOtherDir { .. },
        RegistryIssue::UnknownCheckoutRef { .. },
        RegistryIssue::SelfRef { .. },
        RegistryIssue::RequiresAndConsults { .. },
    ]
);
floor_index!(
    checkout_list,
    CHECKOUT_LIST,
    CheckoutList,
    [CheckoutList::Requires, CheckoutList::Consults]
);

// --- walking the documents ---

fn mark_sessions(seen: &mut Seen, sessions: &[Session]) {
    for s in sessions {
        seen.mark("session_source", session_source(&s.source));
    }
}

fn mark_busy_detection(seen: &mut Seen, s: &Sessions) {
    seen.mark("sessions", sessions(s));
    match s {
        Sessions::Available { unscoped } => mark_sessions(seen, unscoped),
        Sessions::Unavailable { reason } => {
            seen.mark("unavailable", unavailable(reason));
            if let Unavailable::ForeignPidDomain { source, .. } = reason {
                seen.mark("session_source", session_source(source));
            }
        }
    }
}

/// A failure's nested enums, wherever it's carried.
fn mark_failure_detail(seen: &mut Seen, f: &RemoteFailure) {
    match f {
        RemoteFailure::RefGone { fix, .. } => seen.mark("ref_gone_fix", ref_gone_fix(fix)),
        RemoteFailure::Unreachable { cause, .. } => {
            seen.mark("unreachable_cause", unreachable_cause(cause));
        }
        RemoteFailure::RepoNotFound { .. }
        | RemoteFailure::TimedOut { .. }
        | RemoteFailure::Failed { .. }
        | RemoteFailure::Rejected { .. }
        | RemoteFailure::RefspecOutsideOrigin { .. }
        | RemoteFailure::OriginRefsShared { .. }
        | RemoteFailure::LegacyRemotesUnreadable { .. } => {}
    }
}

fn mark_origin_fix(seen: &mut Seen, f: &OriginFix) {
    seen.mark("origin_fix", origin_fix(f));
    match f {
        OriginFix::ByHand { reason } => seen.mark("origin_by_hand", origin_by_hand(reason)),
        OriginFix::Add | OriginFix::SetUrl => {}
    }
}

fn mark_reason(seen: &mut Seen, r: &NeedsHuman) {
    seen.mark("needs_human", needs_human(r));
    match r {
        NeedsHuman::OperationInProgress { op, .. } => seen.mark("in_progress", in_progress(op)),
        NeedsHuman::OriginMismatch { origin, fix, .. } => {
            seen.mark("origin_remote", origin_remote(origin));
            mark_origin_fix(seen, fix);
        }
        NeedsHuman::OriginNotHttps { fix, .. } | NeedsHuman::FetchUrlMismatch { fix, .. } => {
            if let Some(fix) = fix {
                mark_origin_fix(seen, fix);
            }
        }
        NeedsHuman::UnlistedGitDir { head, busy, .. } => {
            seen.mark("unprobed_head", unprobed_head(head));
            mark_sessions(seen, busy);
        }
        NeedsHuman::NotARepo { .. }
        | NeedsHuman::WorktreeUnreadable { .. }
        | NeedsHuman::DefaultBranchMissing { .. }
        | NeedsHuman::DefaultBranchNoUpstream { .. }
        | NeedsHuman::DefaultBranchGone { .. }
        | NeedsHuman::UnexpectedDetached { .. }
        | NeedsHuman::CheckoutUnresolvable { .. }
        | NeedsHuman::PushUrlMismatch { .. }
        | NeedsHuman::CloneSharesRepo { .. }
        | NeedsHuman::ClonedUnregistered { .. } => {}
    }
}

fn mark_report(seen: &mut Seen, report: &StatusReport) {
    mark_busy_detection(seen, &report.sessions);
    for e in &report.entries {
        seen.mark("presence", presence(&e.presence));
        seen.mark("entry_kind", entry_kind(&e.kind));
        if let Some(v) = &e.visibility {
            seen.mark("visibility", visibility(v));
        }
        if let Some(r) = &e.refresh {
            seen.mark("refresh_verdict", refresh_verdict(r));
            if let RefreshVerdict::Held { by } = r {
                seen.mark("held_by/refresh", held_by(by));
            }
        }
        if let Some(c) = &e.clone {
            seen.mark("clone_verdict", clone_verdict(c));
            if let CloneVerdict::Held { by, .. } = c {
                seen.mark("held_by/clone", held_by(by));
            }
        }
        for c in &e.checkouts {
            seen.mark("head", head(&c.head));
            if let Some(op) = &c.in_progress {
                seen.mark("in_progress", in_progress(op));
            }
            mark_sessions(seen, &c.busy);
        }
        for b in &e.branches {
            seen.mark("relation", relation(&b.relation));
            seen.mark("verdict", verdict(&b.verdict));
            match &b.verdict {
                Verdict::Act { action } => seen.mark("verdict_action", sync_action(action)),
                Verdict::Held { action, by } => {
                    seen.mark("verdict_action", SYNC_ACTION + sync_action(action));
                    seen.mark("held_by/branch", held_by(by));
                }
                Verdict::NeedsHuman { reason } => {
                    seen.mark("branch_needs_human", branch_needs_human(reason));
                }
                Verdict::Cleanup { reason, .. } => {
                    seen.mark("cleanup_reason", cleanup_reason(reason));
                }
                Verdict::Quiet | Verdict::LocalOnly => {}
            }
        }
        for r in &e.needs_human {
            mark_reason(seen, r);
        }
        for u in &e.unprobed_worktrees {
            seen.mark("unprobed_head", unprobed_head(&u.worktree.head));
            seen.mark("unprobed_why", unprobed_why(&u.worktree.why));
            if let Some(op) = &u.worktree.in_progress {
                seen.mark("in_progress", in_progress(op));
            }
            if let Some(p) = &u.prune {
                seen.mark("prune", prune(p));
                if let Prune::Loses { losses } = p {
                    for l in losses {
                        seen.mark("prune_loss", prune_loss(l));
                        if let PruneLoss::Operation { op } = l {
                            seen.mark("in_progress", in_progress(op));
                        }
                    }
                }
            }
            mark_sessions(seen, &u.busy);
        }
        if let Some(f) = &e.fetch_error {
            seen.mark("remote_failure/fetch", remote_failure(f));
            mark_failure_detail(seen, f);
        }
        if let Some(v) = &e.visibility_check {
            seen.mark("visibility_check", visibility_check(v));
            if let VisibilityCheck::Unknown { failure } = v {
                mark_failure_detail(seen, failure);
            }
        }
    }
    for s in report.unregistered.iter().flatten() {
        seen.mark("unregistered_kind", unregistered_kind(&s.kind));
        if let UnregisteredKind::MovedWorktree {
            blocked_by: Some(b),
            ..
        } = &s.kind
        {
            seen.mark("repair_block", repair_block(b));
        }
    }
}

fn mark_entry_name(seen: &mut Seen, kind: EntryKind) {
    seen.mark("entry_name_kind", entry_kind(&kind));
}

fn mark_error(seen: &mut Seen, doc: &ErrorReport) {
    seen.mark("error_kind", error_kind(&doc.error.kind));
    let ErrorKind::RegistryInvalid { issues } = &doc.error.kind else {
        return;
    };
    for i in issues {
        seen.mark("registry_issue", registry_issue(i));
        match i {
            RegistryIssue::DirNotAName { entry, .. }
            | RegistryIssue::KeyIsOtherDir { entry, .. } => mark_entry_name(seen, entry.kind),
            RegistryIssue::DirClaimedTwice { first, second, .. } => {
                mark_entry_name(seen, first.kind);
                mark_entry_name(seen, second.kind);
            }
            RegistryIssue::UnknownCheckoutRef { field, .. }
            | RegistryIssue::SelfRef { field, .. } => {
                seen.mark("checkout_list", checkout_list(field));
            }
            RegistryIssue::RepoNotOwned { .. }
            | RegistryIssue::ForkNotOwned { .. }
            | RegistryIssue::KeyInBoth { .. }
            | RegistryIssue::RequiresAndConsults { .. } => {}
        }
    }
}

/// The status documents' every-variant floor: `reports` (the whole
/// workspace's and the targeted one), `detection` (`sessions.json`, every
/// state of busy detection, which a report carries one of), and `errors`
/// (the status error documents) together carry every variant of every
/// closed enum the status report and its error document can hold — each in
/// every place it can. (An entry's `at_rest.followed` repeats the relation
/// its followed branch carries, which the branches' floor covers; a
/// visibility check's failure is floored by its kind alone.)
pub fn assert_status_coverage(
    reports: &[&StatusReport],
    detection: &[Sessions],
    errors: &[&ErrorReport],
) {
    let mut seen = Seen::default();
    for r in reports {
        mark_report(&mut seen, r);
    }
    for s in detection {
        mark_busy_detection(&mut seen, s);
    }
    for e in errors {
        mark_error(&mut seen, e);
    }
    let held = |hs: &[HeldBy]| -> Vec<usize> {
        let can: Vec<usize> = hs.iter().map(held_by).collect();
        (0..HELD_BY).filter(|i| !can.contains(i)).collect()
    };
    // the holds each action can meet
    let branch_holds = [
        HeldBy::Pinned,
        HeldBy::Entry,
        HeldBy::PushUrl,
        HeldBy::FetchFailed,
        HeldBy::DirtyCheckout,
        HeldBy::UnprobedWorktree,
        HeldBy::SeveralCheckouts,
        HeldBy::Busy,
        HeldBy::BusyUnknown,
    ];
    let clone_holds = [HeldBy::Entry, HeldBy::Busy, HeldBy::UnprobedWorktree];
    let refresh_holds = [HeldBy::Pinned, HeldBy::Entry, HeldBy::OriginNotHttps];
    // and every hold is some action's
    let all: BTreeSet<usize> = branch_holds
        .iter()
        .chain(&clone_holds)
        .chain(&refresh_holds)
        .map(held_by)
        .collect();
    assert_eq!(all, (0..HELD_BY).collect(), "a hold no action meets");
    // a push's alone: never a fetch's
    let rejected = remote_failure(&RemoteFailure::Rejected {
        reason: String::new(),
        message: None,
    });
    // with no subcommand no `--json` is known, and the rest are `repos
    // push`'s alone
    let not_status = [
        ErrorKind::MissingCommand,
        ErrorKind::NoCheckout,
        ErrorKind::PushThirdParty { key: String::new() },
        ErrorKind::PushPinned { key: String::new() },
        ErrorKind::NewBranchByAgent,
    ]
    .iter()
    .map(error_kind)
    .collect();
    for (name, n, never) in [
        ("presence", PRESENCE, vec![]),
        ("entry_kind", ENTRY_KIND, vec![]),
        ("visibility", VISIBILITY, vec![]),
        ("head", HEAD, vec![]),
        ("unprobed_head", UNPROBED_HEAD, vec![]),
        ("unprobed_why", UNPROBED_WHY, vec![]),
        ("prune", PRUNE, vec![]),
        ("prune_loss", PRUNE_LOSS, vec![]),
        ("in_progress", IN_PROGRESS, vec![]),
        ("relation", RELATION, vec![]),
        ("verdict", VERDICT, vec![]),
        // act and held, each of every action
        ("verdict_action", 2 * SYNC_ACTION, vec![]),
        ("held_by/branch", HELD_BY, held(&branch_holds)),
        ("held_by/clone", HELD_BY, held(&clone_holds)),
        ("held_by/refresh", HELD_BY, held(&refresh_holds)),
        ("branch_needs_human", BRANCH_NEEDS_HUMAN, vec![]),
        ("cleanup_reason", CLEANUP_REASON, vec![]),
        ("clone_verdict", CLONE_VERDICT, vec![]),
        ("refresh_verdict", REFRESH_VERDICT, vec![]),
        ("needs_human", NEEDS_HUMAN, vec![]),
        ("origin_remote", ORIGIN_REMOTE, vec![]),
        ("origin_fix", ORIGIN_FIX, vec![]),
        ("origin_by_hand", ORIGIN_BY_HAND, vec![]),
        ("remote_failure/fetch", REMOTE_FAILURE, vec![rejected]),
        ("ref_gone_fix", REF_GONE_FIX, vec![]),
        ("unreachable_cause", UNREACHABLE_CAUSE, vec![]),
        ("visibility_check", VISIBILITY_CHECK, vec![]),
        ("sessions", SESSIONS, vec![]),
        ("unavailable", UNAVAILABLE, vec![]),
        ("session_source", SESSION_SOURCE, vec![]),
        ("unregistered_kind", UNREGISTERED_KIND, vec![]),
        ("repair_block", REPAIR_BLOCK, vec![]),
        ("error_kind", ERROR_KIND, not_status),
        ("registry_issue", REGISTRY_ISSUE, vec![]),
        ("entry_name_kind", ENTRY_KIND, vec![]),
        ("checkout_list", CHECKOUT_LIST, vec![]),
    ] {
        seen.floor(name, n, &never);
    }
}

// --- the sync and push documents' enums ---

floor_index!(
    fetch_outcome,
    FETCH_OUTCOME,
    FetchOutcome,
    [
        FetchOutcome::Fetched,
        FetchOutcome::Failed { .. },
        FetchOutcome::NotFetched,
    ]
);
floor_index!(
    branch_outcome,
    BRANCH_OUTCOME,
    BranchOutcome,
    [
        BranchOutcome::Untouched,
        BranchOutcome::NeedsHuman { .. },
        BranchOutcome::Held { .. },
        BranchOutcome::FastForwarded { .. },
        BranchOutcome::Moved { .. },
        BranchOutcome::Pushed { .. },
        BranchOutcome::PushFailed { .. },
        BranchOutcome::Failed { .. },
    ]
);
floor_index!(
    sync_hold,
    SYNC_HOLD,
    SyncHold,
    [
        SyncHold::Pinned,
        SyncHold::Entry,
        SyncHold::PushUrl,
        SyncHold::FetchFailed,
        SyncHold::DirtyCheckout,
        SyncHold::UnprobedWorktree,
        SyncHold::SeveralCheckouts,
        SyncHold::Busy,
        SyncHold::BusyUnknown,
        SyncHold::Changed,
        SyncHold::OriginNotHttps,
    ]
);
floor_index!(
    clone_outcome,
    CLONE_OUTCOME,
    CloneOutcome,
    [
        CloneOutcome::Cloned { .. },
        CloneOutcome::Held { .. },
        CloneOutcome::CloneFailed { .. },
        CloneOutcome::Failed { .. },
    ]
);
floor_index!(
    push_outcome,
    PUSH_OUTCOME,
    PushOutcome,
    [
        PushOutcome::Pushed { .. },
        PushOutcome::InSync,
        PushOutcome::Held { .. },
        PushOutcome::PushFailed { .. },
        PushOutcome::Failed { .. },
        PushOutcome::NotAhead,
        PushOutcome::NeedsHuman { .. },
        PushOutcome::NoUpstream,
        PushOutcome::Detached,
        PushOutcome::Unread,
        PushOutcome::Created { .. },
        PushOutcome::RemoteBranchExists { .. },
    ]
);

/// The sync document's every-variant floor: each outcome, fetch outcome,
/// clone outcome, and hold appears at least once.
pub fn assert_sync_coverage(doc: &SyncReport) {
    let mut seen = Seen::default();
    for e in &doc.entries {
        seen.mark("fetch_outcome", fetch_outcome(&e.fetch));
        if let Some(c) = &e.clone {
            seen.mark("clone_outcome", clone_outcome(c));
        }
        for b in &e.branches {
            seen.mark("branch_outcome", branch_outcome(&b.outcome));
            if let BranchOutcome::Held { by, .. } = &b.outcome {
                seen.mark("sync_hold", sync_hold(by));
            }
        }
    }
    // a refresh's hold alone, never a branch's: the targeted status
    // document carries it, as a refresh verdict's
    let origin_not_https = sync_hold(&SyncHold::OriginNotHttps);
    seen.floor("clone_outcome", CLONE_OUTCOME, &[]);
    seen.floor("branch_outcome", BRANCH_OUTCOME, &[]);
    seen.floor("fetch_outcome", FETCH_OUTCOME, &[]);
    seen.floor("sync_hold", SYNC_HOLD, &[origin_not_https]);
}

/// The push document's every-variant floor: each outcome and fetch
/// outcome appears at least once.
pub fn assert_push_coverage(doc: &PushReport) {
    let mut seen = Seen::default();
    for p in &doc.pushes {
        seen.mark("push_outcome", push_outcome(&p.outcome));
        seen.mark("fetch_outcome", fetch_outcome(&p.fetch));
    }
    seen.floor("push_outcome", PUSH_OUTCOME, &[]);
    seen.floor("fetch_outcome", FETCH_OUTCOME, &[]);
}
