//! Text rendering of a `StatusReport` — the grouped summary,
//! `--verbose`'s per-entry blocks, and `--brief`'s line on one checkout —
//! of a `SyncReport`, whose summary is the same with what sync did in
//! place of what it would do, and of a `PushReport`, its targets' pushes
//! in the same words.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;

use fuz_repos::busy::Sessions;
use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::registry::{EntryKind, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    BranchOutcome, CloneOutcome, EntryStatus, EntrySync, FetchOutcome, PushOutcome, PushReport,
    RepairBlock, StatusReport, SyncHold, SyncReport, UnregisteredClone, UnregisteredKind,
};
use fuz_repos::sessions::{Session, SessionSource, Unavailable};
use fuz_repos::state::{
    BranchNeedsHuman, BranchStatus, Checkout, CleanupReason, CloneVerdict, Head, HeldBy, Presence,
    Prune, PruneLoss, RefreshVerdict, Relation, SyncAction, Uncommitted, UnprobedHead, UnprobedWhy,
    Verdict,
};

/// The label column's width.
const LABEL_WIDTH: usize = 13;

/// The column a group's items start at, and continuation lines hang from.
const ITEM_COLUMN: usize = LABEL_WIDTH + 1;

/// The summary's width when `COLUMNS` doesn't give a usable one.
pub const DEFAULT_WIDTH: usize = 100;

/// The narrowest `COLUMNS` taken as given; below it the default applies.
const MIN_WIDTH: usize = 40;

/// The summary's wrap width from `COLUMNS`: its value when it parses to at
/// least `MIN_WIDTH`, else `DEFAULT_WIDTH`. No terminal-size query — the
/// same environment wraps the same way, piped or not.
pub fn summary_width(columns: Option<&str>) -> usize {
    columns
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|w| *w >= MIN_WIDTH)
        .unwrap_or(DEFAULT_WIDTH)
}

/// Whether to color the summary's labels: only on a terminal, and never
/// when `NO_COLOR` is set to anything but the empty string (no-color.org).
pub fn use_color(is_terminal: bool, no_color: Option<&OsStr>) -> bool {
    is_terminal && no_color.is_none_or(OsStr::is_empty)
}

/// A group label's color; items are never colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// What failed or waits on a person.
    Red,
    /// What sync would do but won't yet, or stops on with a known fix.
    Yellow,
    /// What sync would do.
    Green,
    Plain,
}

impl Tone {
    /// The ANSI SGR sequence that starts it; `None` for plain.
    const fn sgr(self) -> Option<&'static str> {
        match self {
            Self::Red => Some("\x1b[31m"),
            Self::Yellow => Some("\x1b[33m"),
            Self::Green => Some("\x1b[32m"),
            Self::Plain => None,
        }
    }
}

/// A word that POSIX sh and fish both read back as `s`, for the commands the
/// summary and blocks print for a person to run: as is when every char is
/// plainly safe, else single-quoted. Each `'` is written `'\''` and each `\`
/// `'\\'` — closed out of the quotes, since fish honors `\\` and `\'` inside
/// them. Not safe bare: `%` (fish expands `%self`) and `~` (both expand a
/// leading one).
pub fn shell_quote(s: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:@+=,".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return Cow::Borrowed(s);
    }
    let mut quoted = String::with_capacity(s.len() + 2);
    quoted.push('\'');
    for c in s.chars() {
        match c {
            '\'' => quoted.push_str(r"'\''"),
            '\\' => quoted.push_str(r"'\\'"),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    Cow::Owned(quoted)
}

/// A prunable worktree's advice for when it was moved by hand: back where
/// git expects it, or to the workspace root, where the scan reports it with
/// a repair only when that's safe. The hedge exists because the report
/// can't always tell: a worktree moved into the root reads as
/// `Prune::Moved`, but only when the scan ran (no targets given), and one
/// moved anywhere else is out of the scan's sight.
const IF_MOVED: &str =
    "if it moved, move it back (or to the workspace root) and rerun repos status";

/// Why a partial clone's probe may fail, and the fix; `dir` is the checkout
/// as a shell word (`View::show_arg`), or a placeholder. Only `checkout`: a
/// `fetch` backfills a missing tree only as a side effect, and leaves a
/// `--no-checkout` clone without an index, every file then a staged
/// deletion; on a clone already checked out, `checkout` changes nothing.
fn partial_hint(dir: &str) -> String {
    format!(
        "a partial clone may lack objects the probe needs, and repos never fetches them — \
         git -C {dir} checkout fetches them from origin and fills the checkout"
    )
}

/// Why a fetch named a ref the remote no longer has fetched nothing.
const REF_GONE: &str = "a fetch refspec names a branch deleted or renamed on the remote, so \
     nothing was fetched";

/// The summary's `ref_gone` hint: each entry's repair differs.
const REF_GONE_HINT: &str = "a fetch refspec names a branch deleted or renamed on the remote, \
     so nothing was fetched — each entry's repair under --verbose";

/// A `ref_gone` entry's hint: the repair the library decided (`RefGoneFix`),
/// worded, `dir` as in `partial_hint`.
fn ref_gone_hint(fix: &RefGoneFix, dir: &str) -> String {
    match fix {
        RefGoneFix::UnsetRefspec { pattern } => format!(
            "{REF_GONE} — git -C {dir} config --unset-all remote.origin.fetch {} drops just \
             that refspec",
            shell_quote(pattern)
        ),
        RefGoneFix::SetBranches { branch } => format!(
            "{REF_GONE}, and no other refspec in the repo's config would remain — git -C {dir} \
             remote set-branches origin {} points it at a branch the remote has",
            branch
                .as_deref()
                .map_or(Cow::Borrowed("<branch>"), shell_quote)
        ),
        RefGoneFix::ByHand => format!(
            "{REF_GONE}; the refspec naming it is outside the repo's own config (an include, \
             worktree or global config), or none names it as git does — remove it by hand"
        ),
    }
}

/// A host whose SSH key or HTTPS certificate isn't trusted, on fetch.
const HOST_KEY_HINT: &str = "repos never asks to trust a host — check its key (or \
     certificate), then connect once by hand to record it";

/// `REF_GONE_HINT` for `repos push`, which has no `--verbose`.
const PUSH_REF_GONE_HINT: &str = "a fetch refspec names a branch deleted or renamed on the \
     remote, so nothing was fetched — each entry's repair under repos status --fetch --verbose";

/// A branch `repos push` found behind its upstream, or a stale shallow
/// one: sync's to move, never the push's.
const BEHIND_HINT: &str =
    "repos sync fast-forwards a branch behind its upstream (and moves a stale shallow one)";

/// A diverged branch: placing it needs a force-push or a rebase, which
/// repos never does.
const DIVERGED_HINT: &str =
    "a diverged branch is resolved by hand; repos never force-pushes or rebases";

/// A branch with no upstream on origin that `--new-branch` would create:
/// none set, or a same-named one gone. The user's, never an agent's.
const NEW_BRANCH_HINT: &str =
    "the user creates it on origin with repos push --new-branch (an agent can't)";

/// A branch with no upstream on origin that `--new-branch` doesn't create:
/// it tracks another remote, or origin's branch under another name, gone.
const OTHER_UPSTREAM_HINT: &str =
    "--new-branch creates only a same-named origin branch; set others up by hand";

/// A branch whose upstream origin deleted with nothing of it on no remote
/// (merged, most often), so `--new-branch` leaves it be.
const MERGED_HINT: &str = "its commits are all on a remote and origin deleted it: recreating it is by hand, \
     never --new-branch";

/// The branch the entry follows, its upstream gone from origin: the
/// remote's default renamed or deleted, which `--new-branch` never undoes.
const DEFAULT_GONE_HINT: &str = "the entry's own branch is gone from origin (its default renamed?): \
     repoint the registry's branch and the checkout by hand; --new-branch never recreates it";

/// A branch `--new-branch` found origin already has, which it never
/// adopts.
const EXISTS_HINT: &str =
    "set its upstream by hand (git branch -u origin/<branch>), then repos push";

/// A branch whose name origin's fetch refspec maps to no remote-tracking
/// ref, so it can't track a branch created there.
const UNMAPPED_HINT: &str =
    "git remote set-branches --add origin <branch> maps it into the fetch refspec";

/// An HTTPS certificate the visibility check couldn't verify.
const CERTIFICATE_HINT: &str = "the host's HTTPS certificate didn't verify — check it, and the \
     system's CA certificates, then rerun repos status --fetch";

/// A host that refused this machine's credentials, over either transport.
const AUTH_HINT: &str = "the host refused this machine's credentials — over SSH, check the host \
     knows the key and a key with a passphrase is loaded in ssh-agent; over HTTPS, check the \
     credential helper holds a valid token";

/// What rendering needs from the environment: the home dir, shown as `~`;
/// the current time, which the report's timestamps become ages against; the
/// summary's wrap width; and whether its labels are colored.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub home: Option<&'a str>,
    /// Unix seconds.
    pub now: u64,
    /// The summary's wrap width, in chars (`summary_width`).
    pub width: usize,
    /// Color the summary's group labels (`use_color`).
    pub color: bool,
}

impl View<'_> {
    /// A timestamp's compact age.
    fn age(&self, at: u64) -> String {
        format_age(self.now.saturating_sub(at))
    }

    /// What follows the home dir in `path` — empty, or starting with `/` —
    /// when it's under it.
    fn under_home<'p>(&self, path: &'p str) -> Option<&'p str> {
        let home = self.home.filter(|h| !h.is_empty())?;
        path.strip_prefix(home)
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// A path for reading, the home dir shown as `~`.
    pub fn show(&self, path: &str) -> String {
        self.under_home(path)
            .map_or_else(|| path.to_owned(), |rest| format!("~{rest}"))
    }

    /// A path as a word in a command to run: shown as `show` does, and
    /// shell-quoted, with the leading `~/` left outside the quotes so the
    /// shell still expands it.
    pub fn show_arg(&self, path: &str) -> String {
        match self
            .under_home(path)
            .map(|rest| rest.strip_prefix('/').unwrap_or(rest))
        {
            Some("") => "~".to_owned(),
            Some(rest) => format!("~/{}", shell_quote(rest)),
            None => shell_quote(path).into_owned(),
        }
    }
}

/// The grouped summary: what to act on, the quiet entries as counts, and the
/// footer. A workspace with nothing to act on prints just the last line.
pub fn render_summary(report: &StatusReport, view: View<'_>, verbose: bool) -> String {
    summary(report, None, view, verbose)
}

/// `repos sync`'s summary: the status summary of what it acted on, with
/// what it did in place of `sync would` — `synced`, and on `held` and
/// `failed` what it didn't.
pub fn render_sync_summary(report: &SyncReport, view: View<'_>, verbose: bool) -> String {
    summary(&report.status, Some(&report.entries), view, verbose)
}

/// `repos push`'s summary: what it did with each target's branch —
/// `pushed`, `in sync`, `held`, `not pushed` — after what failed and what's
/// a person's (the targets' entries' own reasons among them, so a hold on
/// the entry is explained), the hints that say what to do next, and the
/// registry line. Worded as `sync`'s, a branch labeled by its entry's key
/// alone when it's the registry's branch.
pub fn render_push_summary(report: &PushReport, view: View<'_>) -> String {
    let mut visibility = Vec::new();
    let mut failed = Vec::new();
    let mut needs_human = Vec::new();
    let mut origin_drift = Vec::new();
    let mut pushed = Vec::new();
    let mut in_sync = Vec::new();
    let mut held = Vec::new();
    let mut not_pushed = Vec::new();
    // the guard itself failed: said once, first among the failures
    if let Sessions::Unavailable { reason } = &report.status.sessions {
        failed.push(format!(
            "busy detection ({}; every push held)",
            unavailable_label(reason, view)
        ));
    }
    for e in &report.status.entries {
        let key = &e.key;
        if let Some(error) = &e.probe_error {
            failed.push(format!("{key} (probe: {})", first_line(error)));
        }
        if let Some(failure) = &e.fetch_error {
            failed.push(format!(
                "{key} (fetch: {})",
                remote_failure_label(failure, false)
            ));
        }
        match &e.visibility_check {
            Some(VisibilityCheck::Leak) => {
                visibility.push(format!("{key} (declared private, anonymously readable)"));
            }
            Some(VisibilityCheck::Unknown { failure }) => failed.push(format!(
                "{key} (visibility check: {})",
                remote_failure_label(failure, false)
            )),
            Some(VisibilityCheck::Private) | None => {}
        }
        for reason in &e.needs_human {
            match reason {
                NeedsHuman::OriginMismatch { origin, .. } => {
                    let was = match origin {
                        OriginRemote::Url { url } => compact_remote(url, &e.url),
                        OriginRemote::NoUrl => "origin has no URL".to_owned(),
                        OriginRemote::Missing => "no origin".to_owned(),
                    };
                    origin_drift.push(format!("{key} ({was})"));
                }
                reason => {
                    needs_human.push(format!("{key} ({})", needs_human_label(reason, e, view)));
                }
            }
        }
    }
    let (mut behind, mut diverged, mut unmapped, mut exists) = (false, false, false, false);
    // branches with no upstream on origin: ones --new-branch creates, merged
    // ones, and the rest
    let (mut new_branch, mut merged, mut own_gone, mut other_upstream) =
        (false, false, false, false);
    for p in &report.pushes {
        let Some(e) = report.status.entries.iter().find(|e| e.key == p.key) else {
            failed.push(format!("{} (push: no status)", p.key));
            continue;
        };
        let b = p
            .branch
            .as_ref()
            .and_then(|name| e.branches.iter().find(|b| b.name == *name));
        let label = match &p.branch {
            Some(name) if Some(name) != e.branch.as_ref() => format!("{}:{name}", p.key),
            _ => p.key.clone(),
        };
        // a checkout with no branch to name it by: its path, when it isn't
        // the entry's dir
        let at = if Path::new(&p.checkout) == Path::new(&report.status.workspace).join(&e.dir) {
            String::new()
        } else {
            format!(", worktree {}", view.show(&p.checkout))
        };
        let ahead = match b.map(|b| b.relation) {
            Some(Relation::Ahead { commits }) => format!(" +{commits}"),
            _ => String::new(),
        };
        match &p.outcome {
            PushOutcome::Pushed { .. } => pushed.push(format!("{label}{ahead}")),
            PushOutcome::Created { .. } => pushed.push(format!("{label} (new branch)")),
            PushOutcome::RemoteBranchExists { at } => {
                exists = true;
                needs_human.push(format!(
                    "{label} (on origin already, at {})",
                    at.get(..7).unwrap_or(at)
                ));
            }
            PushOutcome::InSync => in_sync.push(label),
            PushOutcome::Held { by } => held.push(format!("{label}{ahead}{}", hold_note(*by))),
            PushOutcome::PushFailed { failure } => failed.push(format!(
                "{label} (push: {})",
                remote_failure_label(failure, false)
            )),
            PushOutcome::Failed { message } => {
                failed.push(format!("{label} (push: {})", first_line(message)));
            }
            PushOutcome::NotAhead => {
                behind = true;
                let relation =
                    b.map_or_else(|| "not ahead".to_owned(), |b| relation_label(b.relation));
                not_pushed.push(format!("{label} ({relation})"));
            }
            PushOutcome::NeedsHuman { reason } => {
                diverged |= *reason == BranchNeedsHuman::Diverged;
                unmapped |= *reason == BranchNeedsHuman::Unmapped;
                let why = b.map_or_else(
                    || format!("{reason:?}"),
                    |b| branch_needs_human_label(*reason, b),
                );
                needs_human.push(format!("{label} ({why})"));
            }
            PushOutcome::NoUpstream => {
                let default_gone = b.is_some_and(|b| e.default_branch_gone(&b.name));
                let creatable = b.is_some_and(|b| new_branch_creates(e, b));
                let gone_merged = !default_gone
                    && b.is_some_and(|b| b.relation == Relation::Gone && b.unique_commits == 0);
                new_branch |= creatable;
                merged |= gone_merged;
                own_gone |= default_gone;
                other_upstream |= !creatable && !gone_merged && !default_gone;
                let why = match b.map(|b| (b.relation, b.upstream.as_deref())) {
                    Some((Relation::Gone, _)) if default_gone => {
                        "the entry's branch, upstream gone from origin".to_owned()
                    }
                    Some((Relation::Gone, _)) if gone_merged => {
                        "nothing unique, upstream gone from origin".to_owned()
                    }
                    Some((Relation::Gone, _)) => "upstream gone from origin".to_owned(),
                    Some((Relation::Untracked, Some(upstream))) => format!("tracks {upstream}"),
                    _ => "no upstream on origin".to_owned(),
                };
                not_pushed.push(format!("{label} ({why})"));
            }
            PushOutcome::Detached => not_pushed.push(format!("{label} (detached HEAD{at})")),
            PushOutcome::Unread => {
                let why = match e.presence {
                    Presence::Missing => "missing",
                    Presence::NotARepo => "not a repo",
                    Presence::Present if e.probe_error.is_some() => "probe failed",
                    Presence::Present => "checkout not read",
                };
                not_pushed.push(format!("{label} ({why}{at})"));
            }
        }
    }

    let mut out = String::new();
    let mut line = |label: &str, tone: Tone, items: Vec<String>| {
        out.push_str(&render_group(label, tone, &Items::Singles(items), view));
    };
    let hint = |s: &str| vec![format!("hint: {s}")];
    line("visibility", Tone::Red, visibility);
    line("failed", Tone::Red, failed);
    // a fetch's failure, or a push's
    let remote_failed = |pick: fn(&RemoteFailure) -> bool| {
        report
            .status
            .entries
            .iter()
            .any(|e| e.fetch_error.as_ref().is_some_and(pick))
            || report.pushes.iter().any(|p| match &p.outcome {
                PushOutcome::PushFailed { failure } => pick(failure),
                _ => false,
            })
    };
    if remote_failed(|f| matches!(f, RemoteFailure::RefGone { .. })) {
        line("", Tone::Plain, hint(PUSH_REF_GONE_HINT));
    }
    if remote_failed(|f| unreachable_cause(f) == Some(UnreachableCause::HostKey)) {
        line("", Tone::Plain, hint(HOST_KEY_HINT));
    }
    if remote_failed(|f| unreachable_cause(f) == Some(UnreachableCause::Auth)) {
        line("", Tone::Plain, hint(AUTH_HINT));
    }
    line("needs human", Tone::Red, needs_human);
    if diverged {
        line("", Tone::Plain, hint(DIVERGED_HINT));
    }
    if unmapped {
        line("", Tone::Plain, hint(UNMAPPED_HINT));
    }
    if exists {
        line("", Tone::Plain, hint(EXISTS_HINT));
    }
    line("origin drift", Tone::Yellow, origin_drift);
    line("pushed", Tone::Green, pushed);
    line("in sync", Tone::Plain, in_sync);
    line("held", Tone::Yellow, held);
    line("not pushed", Tone::Yellow, not_pushed);
    if behind {
        line("", Tone::Plain, hint(BEHIND_HINT));
    }
    if new_branch {
        line("", Tone::Plain, hint(NEW_BRANCH_HINT));
    }
    if merged {
        line("", Tone::Plain, hint(MERGED_HINT));
    }
    if own_gone {
        line("", Tone::Plain, hint(DEFAULT_GONE_HINT));
    }
    if other_upstream {
        line("", Tone::Plain, hint(OTHER_UPSTREAM_HINT));
    }
    let _ = writeln!(out, "{}", footer(&report.status, view));
    out
}

/// Whether `repos push --new-branch` would create `b` on origin, as its
/// status reads: no upstream configured, or origin's same-named branch as
/// its upstream, gone, with commits on no remote (with none, it was
/// merged) — never the branch the entry follows (`default_branch_gone`).
/// (The run itself reads the merge ref as git resolves it.)
fn new_branch_creates(e: &EntryStatus, b: &BranchStatus) -> bool {
    match (b.relation, b.upstream.as_deref()) {
        (Relation::Untracked, upstream) => upstream.is_none(),
        (Relation::Gone, _) if e.default_branch_gone(&b.name) => false,
        (Relation::Gone, Some(upstream)) => {
            b.unique_commits > 0 && upstream == format!("origin/{}", b.name)
        }
        _ => false,
    }
}

/// `status --brief`'s one line on the checkout `c` of `e`, for a session
/// starting in it: `None` when there's nothing to say, or when the probe
/// failed (its facts are incomplete).
///
/// It says, in order: the other live sessions working in the checkout
/// itself (its `working`: by where they are, or by the lock Claude Code put
/// on it for them — not a session elsewhere in the repo, which `busy` adds
/// for the agent worktrees under `.claude/worktrees/`; and none when busy
/// detection is unavailable — a nudge doesn't guess); an operation in
/// progress in it; and the relation of the branch it's on to that branch's
/// origin upstream — behind (with how old the remote view is) or diverged,
/// then ahead (unpushed). Behind and ahead only where every fetching run
/// fetches, an owned entry that isn't pinned: a third-party reference is
/// fetched only when asked, and a pin never is, its consumer moving it.
/// Every other relation — in sync, gone, shallow, unmapped, untracked — and
/// a detached HEAD say nothing, and neither does dirt, which the session
/// sees for itself. Plain text on one line, never wrapped or colored.
pub fn render_brief(e: &EntryStatus, c: &Checkout, view: View<'_>) -> Option<String> {
    if e.probe_error.is_some() {
        return None;
    }
    let mut items = Vec::new();
    match c.working.len() {
        0 => {}
        1 => items.push("another live session is working in this checkout".to_owned()),
        n => items.push(format!(
            "{n} other live sessions are working in this checkout"
        )),
    }
    if let Some(op) = c.in_progress {
        items.push(format!("{} in progress", op.label()));
    }
    let branch = match &c.head {
        Head::Branch { name } if e.writable && !e.pinned => {
            e.branches.iter().find(|b| b.name == *name)
        }
        _ => None,
    };
    if let Some(b) = branch {
        // a relation to origin implies an origin upstream
        let upstream = b.upstream.as_deref().unwrap_or("origin");
        let fetched = e
            .fetched_at
            .map(|at| format!(" (fetched {} ago)", view.age(at)))
            .unwrap_or_default();
        match b.relation {
            Relation::Behind { commits } => {
                items.push(format!("{commits} behind {upstream}{fetched}"));
            }
            Relation::Diverged { ahead, behind } => {
                items.push(format!(
                    "diverged from {upstream} +{ahead} −{behind}{fetched}"
                ));
            }
            Relation::Ahead { commits } => {
                items.push(format!("{commits} ahead of {upstream} (unpushed)"));
            }
            Relation::InSync
            | Relation::Shallow
            | Relation::Gone
            | Relation::Unmapped
            | Relation::Untracked => {}
        }
    }
    (!items.is_empty()).then(|| format!("repos: {} — {}\n", e.key, items.join("; ")))
}

/// The summary of `report`, and with `synced` (one per entry, in order) of
/// what sync did.
fn summary(
    report: &StatusReport,
    synced: Option<&[EntrySync]>,
    view: View<'_>,
    verbose: bool,
) -> String {
    let mut g = Groups {
        synced: synced.is_some(),
        ..Groups::default()
    };
    let mut quiet = Counts::default();
    let workspace = Path::new(&report.workspace);
    for (i, e) in report.entries.iter().enumerate() {
        let sync = synced.and_then(|s| s.get(i));
        if g.add(e, sync, workspace, view, verbose) {
            continue;
        }
        // a quiet entry off its followed branch is on another one: detached
        // off it is an `unexpected_detached` reason, or its operation's
        match (e.pinned, e.at_rest.and_then(|r| r.on_branch)) {
            (true, _) => quiet.pinned += 1,
            (false, Some(false)) => quiet.on_branches += 1,
            (false, Some(true) | None) => quiet.clean += 1,
        }
    }

    // the guard itself failed: said once, first among the failures
    if let Sessions::Unavailable { reason } = &report.sessions {
        g.failed.insert(
            0,
            format!(
                "busy detection ({}; every push, ff, and move held)",
                unavailable_label(reason, view)
            ),
        );
    }
    let mut unscoped = Vec::new();
    if let (true, Sessions::Available { unscoped: sessions }) = (verbose, &report.sessions) {
        unscoped.extend(sessions.iter().map(|s| session_label(s, view)));
    }

    let mut out = String::new();
    let mut line = |label: &str, tone: Tone, items: Items| {
        out.push_str(&render_group(label, tone, &items, view));
    };
    line("visibility", Tone::Red, Items::Singles(g.visibility));
    line("failed", Tone::Red, Items::Singles(g.failed));
    let mut hints = Vec::new();
    if report.entries.iter().any(EntryStatus::probe_failed_partial) {
        hints.push(format!("{} (each under --verbose)", partial_hint("<dir>")));
    }
    // a fetch's failure, or under sync a push's or a clone's
    let fetch_failed = |pick: fn(&RemoteFailure) -> bool| {
        report
            .entries
            .iter()
            .any(|e| e.fetch_error.as_ref().is_some_and(pick))
            || synced.into_iter().flatten().any(|e| {
                matches!(&e.clone, Some(CloneOutcome::CloneFailed { failure }) if pick(failure))
                    || e.branches.iter().any(|b| match &b.outcome {
                        BranchOutcome::PushFailed { failure } => pick(failure),
                        _ => false,
                    })
            })
    };
    if fetch_failed(|f| matches!(f, RemoteFailure::RefGone { .. })) {
        hints.push(REF_GONE_HINT.to_owned());
    }
    if fetch_failed(|f| unreachable_cause(f) == Some(UnreachableCause::HostKey)) {
        hints.push(HOST_KEY_HINT.to_owned());
    }
    if fetch_failed(|f| unreachable_cause(f) == Some(UnreachableCause::Auth)) {
        hints.push(AUTH_HINT.to_owned());
    }
    if report
        .entries
        .iter()
        .any(|e| visibility_cause(e) == Some(UnreachableCause::HostKey))
    {
        hints.push(CERTIFICATE_HINT.to_owned());
    }
    for hint in hints {
        line(
            "",
            Tone::Plain,
            Items::Singles(vec![format!("hint: {hint}")]),
        );
    }
    line("needs human", Tone::Red, Items::Singles(g.needs_human));
    line("origin drift", Tone::Yellow, Items::Singles(g.origin_drift));
    // the fixes the drifts call for, each worded once
    let mut fixes: Vec<&str> = Vec::new();
    for fix in report
        .entries
        .iter()
        .flat_map(|e| &e.needs_human)
        .filter_map(|r| match r {
            NeedsHuman::OriginMismatch { fix, .. } => Some(fix),
            _ => None,
        })
    {
        let words = match fix {
            OriginFix::SetUrl => "git -C <dir> remote set-url origin <url>",
            OriginFix::Add => "git -C <dir> remote add origin <url>",
            OriginFix::ByHand { .. } => "remote.origin.url by hand",
        };
        if !fixes.contains(&words) {
            fixes.push(words);
        }
    }
    if !fixes.is_empty() {
        let hint = format!("hint: {} (each under --verbose)", fixes.join(", or "));
        line("", Tone::Plain, Items::Singles(vec![hint]));
    }
    let act_label = if synced.is_some() {
        "synced"
    } else {
        "sync would"
    };
    line(act_label, Tone::Green, Items::Runs(g.act.verbs(), " · "));
    line("held", Tone::Yellow, Items::Runs(g.held.verbs(), " · "));
    line("local-only", Tone::Plain, Items::Singles(g.local_only));
    line("uncommitted", Tone::Plain, Items::Singles(g.uncommitted));
    line("cleanup", Tone::Plain, Items::Singles(g.cleanup));
    line(
        "unregistered",
        Tone::Plain,
        Items::Runs(unregistered_groups(report, view), "  "),
    );
    line("stashes", Tone::Plain, Items::Singles(g.stashes));
    line("unscoped", Tone::Plain, Items::Singles(unscoped));

    let counts = format!(
        "clean {} · on branches {} · pinned {}",
        quiet.clean, quiet.on_branches, quiet.pinned
    );
    let _ = writeln!(out, "{counts}      {}", footer(report, view));
    out
}

#[derive(Debug, Default)]
struct Counts {
    clean: u32,
    on_branches: u32,
    pinned: u32,
}

/// A summary group's items.
#[derive(Debug)]
enum Items {
    /// Items that stand apart, two spaces between them.
    Singles(Vec<String>),
    /// Runs of items — a verb's branches, an ownership's strays — each run's
    /// items joined by `, `, the runs by the separator.
    Runs(Vec<Vec<String>>, &'static str),
}

/// Items as one run, the first carrying `prefix` (`push `, `owned: `); no
/// run when there are none.
fn prefixed(prefix: &str, mut items: Vec<String>) -> Option<Vec<String>> {
    let first = items.first_mut()?;
    first.insert_str(0, prefix);
    Some(items)
}

/// One summary group: its label, then its items from `ITEM_COLUMN`,
/// wrapped at `view.width` with a hanging indent; nothing when there are no
/// items.
///
/// A line breaks only between items, never inside one: an item too long for
/// the width stands alone on its line. Singles flow, as many to a line as
/// fit. Runs that fit on one line share it; otherwise each run starts its
/// own line (the separator dropped, the run's prefix leading it) and its
/// items flow from there, a break inside a run leaving the `,` at the line's
/// end.
fn render_group(label: &str, tone: Tone, items: &Items, view: View<'_>) -> String {
    // each word with what joins it to the one before on the same line, and
    // whether it opens a run
    let mut words: Vec<(&str, Cow<'_, str>, bool)> = Vec::new();
    match items {
        Items::Singles(items) => {
            words.extend(
                items
                    .iter()
                    .map(|item| ("  ", Cow::Borrowed(item.as_str()), false)),
            );
        }
        Items::Runs(runs, sep) => {
            for run in runs {
                for (i, item) in run.iter().enumerate() {
                    let word = if i + 1 < run.len() {
                        Cow::Owned(format!("{item},"))
                    } else {
                        Cow::Borrowed(item.as_str())
                    };
                    words.push(if i == 0 {
                        (sep, word, true)
                    } else {
                        (" ", word, false)
                    });
                }
            }
        }
    }
    if words.is_empty() {
        return String::new();
    }
    let chars = |s: &str| s.chars().count();
    let one_line = ITEM_COLUMN
        + words
            .iter()
            .enumerate()
            .map(|(i, (join, word, _))| if i == 0 { 0 } else { chars(join) } + chars(word))
            .sum::<usize>();
    let run_per_line = one_line > view.width;

    let mut out = tone
        .sgr()
        .filter(|_| view.color)
        .map_or_else(|| label.to_owned(), |sgr| format!("{sgr}{label}\x1b[0m"));
    let pad = ITEM_COLUMN.saturating_sub(chars(label)).max(1);
    out.extend(std::iter::repeat_n(' ', pad));
    let mut column = ITEM_COLUMN;
    for (i, (join, word, opens_run)) in words.iter().enumerate() {
        if i > 0 {
            if (run_per_line && *opens_run) || column + chars(join) + chars(word) > view.width {
                out.push('\n');
                out.extend(std::iter::repeat_n(' ', ITEM_COLUMN));
                column = ITEM_COLUMN;
            } else {
                out.push_str(join);
                column += chars(join);
            }
        }
        out.push_str(word);
        column += chars(word);
    }
    out.push('\n');
    out
}

/// Sync actions by verb, each item a labeled branch — or, for a refresh or
/// a clone, an entry's key.
#[derive(Debug, Default)]
struct Actions {
    refreshes: Vec<String>,
    push: Vec<String>,
    ff: Vec<String>,
    moves: Vec<String>,
    clones: Vec<String>,
}

impl Actions {
    /// Adds a labeled branch's action; `note` follows it, e.g. ` (dirty)`.
    fn add(&mut self, action: SyncAction, label: &str, note: &str) {
        match action {
            SyncAction::Push { commits } => self.push.push(format!("{label} +{commits}{note}")),
            SyncAction::FastForward { commits } => {
                self.ff.push(format!("{label} −{commits}{note}"));
            }
            SyncAction::Move => self.moves.push(format!("{label}{note}")),
        }
    }

    /// Adds a missing entry's clone, by its key; `note` as for `add`.
    fn add_clone(&mut self, key: &str, note: &str) {
        self.clones.push(format!("{key}{note}"));
    }

    /// Adds a reference's refresh, by its key; `note` as for `add`.
    fn add_refresh(&mut self, key: &str, note: &str) {
        self.refreshes.push(format!("{key}{note}"));
    }

    /// A run per verb — `refresh lib`, `push a +1, b +2`, `ff …`, `move …`,
    /// `clone …` — omitting empty verbs.
    fn verbs(&self) -> Vec<Vec<String>> {
        [
            ("refresh ", &self.refreshes),
            ("push ", &self.push),
            ("ff ", &self.ff),
            ("move ", &self.moves),
            ("clone ", &self.clones),
        ]
        .into_iter()
        .filter_map(|(verb, items)| prefixed(verb, items.clone()))
        .collect()
    }

    const fn len(&self) -> usize {
        self.refreshes.len()
            + self.push.len()
            + self.ff.len()
            + self.moves.len()
            + self.clones.len()
    }
}

#[derive(Debug, Default)]
struct Groups {
    /// A sync report's: `act` is what sync did.
    synced: bool,
    visibility: Vec<String>,
    failed: Vec<String>,
    needs_human: Vec<String>,
    origin_drift: Vec<String>,
    act: Actions,
    held: Actions,
    local_only: Vec<String>,
    uncommitted: Vec<String>,
    cleanup: Vec<String>,
    stashes: Vec<String>,
}

impl Groups {
    /// Adds an entry's lines; returns whether it had anything to say. In a
    /// sync report, `sync` holds the entry's outcomes, and a branch's action
    /// reads as what sync did — `act` what it did, `held` and `failed` what
    /// it didn't.
    fn add(
        &mut self,
        e: &EntryStatus,
        sync: Option<&EntrySync>,
        workspace: &Path,
        view: View<'_>,
        verbose: bool,
    ) -> bool {
        let before = self.len();
        let key = &e.key;
        let label = |b: &BranchStatus| {
            if Some(&b.name) == e.branch.as_ref() {
                key.clone()
            } else {
                format!("{key}:{}", b.name)
            }
        };

        if let Some(error) = &e.probe_error {
            self.failed
                .push(format!("{key} (probe: {})", first_line(error)));
        }
        if let Some(failure) = &e.fetch_error {
            self.failed.push(format!(
                "{key} (fetch: {})",
                remote_failure_label(failure, false)
            ));
        }
        match &e.visibility_check {
            Some(VisibilityCheck::Leak) => self
                .visibility
                .push(format!("{key} (declared private, anonymously readable)")),
            Some(VisibilityCheck::Unknown { failure }) => self.failed.push(format!(
                "{key} (visibility check: {})",
                remote_failure_label(failure, false)
            )),
            Some(VisibilityCheck::Private) | None => {}
        }
        for reason in &e.needs_human {
            match reason {
                NeedsHuman::OriginMismatch { origin, .. } => {
                    let was = match origin {
                        OriginRemote::Url { url } => compact_remote(url, &e.url),
                        OriginRemote::NoUrl => "origin has no URL".to_owned(),
                        OriginRemote::Missing => "no origin".to_owned(),
                    };
                    self.origin_drift.push(format!("{key} ({was})"));
                }
                reason => self
                    .needs_human
                    .push(format!("{key} ({})", needs_human_label(reason, e, view))),
            }
        }
        // a reference asked for by name or `--references`: under sync, a
        // refresh is its fetch (a failed one is said above, as failed)
        match (&e.refresh, sync.map(|s| &s.fetch)) {
            (Some(RefreshVerdict::Act), None | Some(FetchOutcome::Fetched)) => {
                self.act.add_refresh(key, "");
            }
            (Some(RefreshVerdict::Held { by }), _) => {
                self.held
                    .add_refresh(key, refresh_held_note(*by, &e.needs_human));
            }
            (Some(RefreshVerdict::Act), Some(_)) | (None, _) => {}
        }
        match (&e.clone, self.synced) {
            (Some(_), true) => {
                self.add_clone_outcome(key, sync.and_then(|s| s.clone.as_ref()));
            }
            (Some(CloneVerdict::Act { .. }), false) => self.act.add_clone(key, ""),
            (Some(CloneVerdict::Held { by, .. }), false) => {
                self.held.add_clone(key, held_note(*by));
            }
            (None, _) => {}
        }
        for (bi, b) in e.branches.iter().enumerate() {
            if self.synced && matches!(b.verdict, Verdict::Act { .. } | Verdict::Held { .. }) {
                let synced = sync.and_then(|s| s.branches.get(bi));
                // another entry sharing the repo says it, once
                if synced.is_some_and(|s| s.repeats.is_some()) {
                    continue;
                }
                self.add_outcome(b, synced.map(|s| &s.outcome), &label(b));
                continue;
            }
            match &b.verdict {
                // a pin is the consumer's standing choice, not a hold to
                // clear: the entry counts as pinned, and `--verbose` shows
                // what it holds
                Verdict::Quiet
                | Verdict::Held {
                    by: HeldBy::Pinned, ..
                } => {}
                Verdict::Act { action } => self.act.add(*action, &label(b), ""),
                Verdict::Held { action, by } => {
                    self.held.add(*action, &label(b), held_note(*by));
                }
                Verdict::NeedsHuman { reason } => {
                    self.needs_human.push(format!(
                        "{} ({})",
                        label(b),
                        branch_needs_human_label(*reason, b)
                    ));
                }
                Verdict::LocalOnly => {
                    let read_only = if e.writable { "" } else { ", read-only" };
                    self.local_only.push(format!(
                        "{} (+{}, {}{read_only})",
                        label(b),
                        b.unique_commits,
                        view.age(b.newest_commit_at)
                    ));
                }
                Verdict::Cleanup {
                    reason,
                    removable_worktree,
                } => {
                    let mut why = match reason {
                        CleanupReason::UpstreamGone if b.unique_commits > 0 => {
                            format!("upstream gone, +{}", b.unique_commits)
                        }
                        CleanupReason::UpstreamGone => "upstream gone".to_owned(),
                        CleanupReason::Merged => "merged".to_owned(),
                    };
                    if let Some(path) = removable_worktree {
                        let _ = write!(why, ", worktree {} removable", view.show(path));
                    }
                    self.cleanup.push(format!("{} ({why})", label(b)));
                }
            }
        }
        // a git dir that can't be read is said once, as the needs-human
        // reason that holds the entry — present whether or not a worktree
        // in it was listed — never again as that worktree's failed probe
        let unreadable = |path: &str| {
            e.needs_human.iter().any(|r| {
                matches!(r, NeedsHuman::WorktreeUnreadable { path: p }
                    if Path::new(path).starts_with(p))
            })
        };
        for u in &e.unprobed_worktrees {
            let at = view.show(&u.worktree.path);
            match (&u.worktree.why, &u.prune) {
                (UnprobedWhy::Failed { .. }, _) if unreadable(&u.worktree.path) => {}
                (UnprobedWhy::Failed { error }, _) => self
                    .failed
                    .push(format!("{key} (worktree {at}: {})", first_line(error))),
                // never `git worktree repair <new path>` or `git worktree
                // prune`: both are repo-wide, the one may hijack another
                // checkout and the other drops every gone worktree, so the
                // command is `remove`, this one's alone, and only the scan
                // offers a repair, vetted, for a moved worktree at the root
                (UnprobedWhy::Prunable, Some(Prune::Safe)) => self.cleanup.push(format!(
                    "{key} (worktree {at} gone — {IF_MOVED}, else git -C {} worktree remove {})",
                    view.show_arg(&workspace.join(&e.dir).to_string_lossy()),
                    view.show_arg(&u.worktree.path)
                )),
                // the scan found it moved: those strays' lines say what to
                // do, whatever they are, and removing it would orphan them
                (UnprobedWhy::Prunable, Some(Prune::Moved { to })) => {
                    let see = if to.len() == 1 {
                        "its line"
                    } else {
                        "their lines"
                    };
                    self.cleanup.push(format!(
                        "{key} (worktree {at} gone — moved to {}; see {see})",
                        to.join(", ")
                    ));
                }
                // classify found removing it would lose something: word it
                (UnprobedWhy::Prunable, loses) => {
                    let losses = match loses {
                        Some(Prune::Loses { losses }) => {
                            losses.iter().map(prune_loss_label).collect::<Vec<_>>()
                        }
                        _ => vec!["its state".to_owned()],
                    };
                    self.cleanup.push(format!(
                        "{key} (worktree {at} gone — {IF_MOVED}; removing discards {})",
                        losses.join(" and ")
                    ));
                }
                // intentional, as on unmounted media: `--verbose` shows it,
                // and it still holds its branch's fast-forward and move (its
                // push too, when a session works in its files wherever
                // they're mounted)
                (UnprobedWhy::Missing, _) => {}
            }
        }
        if verbose {
            // each dirty checkout its own item, another worktree by its
            // shown path beside the primary's key
            for c in e.checkouts.iter().filter(|c| !c.uncommitted.is_clean()) {
                let detail = uncommitted_detail(&c.uncommitted);
                self.uncommitted.push(if c.primary {
                    format!("{key} ({detail})")
                } else {
                    format!("{key} (worktree {}, {detail})", view.show(&c.path))
                });
            }
        } else if let Some(item) = uncommitted_summary(key, &e.checkouts, view) {
            self.uncommitted.push(item);
        }
        let said = self.len() > before;
        if verbose && e.stashes > 0 {
            self.stashes.push(format!("{key} ({})", e.stashes));
        }
        said
    }

    /// A missing entry's clone, as what sync did: `outcome` is `None` when
    /// the report carries none for it.
    fn add_clone_outcome(&mut self, key: &str, outcome: Option<&CloneOutcome>) {
        match outcome {
            Some(CloneOutcome::Cloned { .. }) => self.act.add_clone(key, ""),
            Some(CloneOutcome::Held { by }) => self.held.add_clone(key, hold_note(*by)),
            Some(CloneOutcome::CloneFailed { failure }) => self.failed.push(format!(
                "{key} (clone: {})",
                remote_failure_label(failure, false)
            )),
            Some(CloneOutcome::Failed { message }) => self
                .failed
                .push(format!("{key} (clone: {})", first_line(message))),
            None => self.failed.push(format!("{key} (clone: no outcome)")),
        }
    }

    /// A branch sync would act on, as what it did: `outcome` is `None`
    /// when the report carries none for it.
    fn add_outcome(&mut self, b: &BranchStatus, outcome: Option<&BranchOutcome>, label: &str) {
        let action = match (&b.verdict, outcome) {
            (Verdict::Act { action } | Verdict::Held { action, .. }, _) => *action,
            _ => return,
        };
        match outcome {
            Some(
                BranchOutcome::FastForwarded { .. }
                | BranchOutcome::Moved { .. }
                | BranchOutcome::Pushed { .. },
            ) => {
                self.act.add(action, label, "");
            }
            // a pin is a standing choice: counted, not held
            Some(
                BranchOutcome::Held {
                    by: SyncHold::Pinned,
                    ..
                }
                | BranchOutcome::Untouched,
            ) => {}
            Some(BranchOutcome::Held { action, by }) => {
                self.held.add(*action, label, hold_note(*by));
            }
            Some(BranchOutcome::PushFailed { failure }) => {
                self.failed.push(format!(
                    "{label} (push: {})",
                    remote_failure_label(failure, false)
                ));
            }
            Some(BranchOutcome::Failed { action, message }) => {
                self.failed
                    .push(format!("{label} ({}: {message})", action_verb(*action)));
            }
            Some(BranchOutcome::NeedsHuman { reason }) => {
                self.needs_human.push(format!(
                    "{label} ({})",
                    branch_needs_human_label(*reason, b)
                ));
            }
            None => self
                .failed
                .push(format!("{label} ({}: no outcome)", action_verb(action))),
        }
    }

    const fn len(&self) -> usize {
        self.visibility.len()
            + self.failed.len()
            + self.needs_human.len()
            + self.origin_drift.len()
            + self.act.len()
            + self.held.len()
            + self.local_only.len()
            + self.uncommitted.len()
            + self.cleanup.len()
    }
}

/// Why a branch needs a person, with its relation's counts.
fn branch_needs_human_label(reason: BranchNeedsHuman, b: &BranchStatus) -> String {
    match (reason, b.relation) {
        (BranchNeedsHuman::Diverged, Relation::Diverged { ahead, behind }) => {
            format!("diverged +{ahead} −{behind}")
        }
        (BranchNeedsHuman::Diverged, _) => "diverged".into(),
        (BranchNeedsHuman::Unmapped, _) => "outside refspec".into(),
        (BranchNeedsHuman::ArchivedAhead, Relation::Ahead { commits }) => {
            format!("archived, +{commits}")
        }
        (BranchNeedsHuman::ArchivedAhead, _) => "archived, ahead".into(),
        (BranchNeedsHuman::ShallowLocalWork, _) => {
            format!("shallow, tips differ, +{} local", b.unique_commits)
        }
        (BranchNeedsHuman::UpstreamNotABranch, _) => format!(
            "ahead, upstream {} not a branch",
            b.upstream.as_deref().unwrap_or("unknown")
        ),
    }
}

/// An unprobed checkout's HEAD, after its path.
fn unprobed_head_label(head: &UnprobedHead) -> String {
    match head {
        UnprobedHead::Branch { name } => format!(" on {name}"),
        UnprobedHead::Detached { commit } => {
            format!(" detached at {}", commit.get(..12).unwrap_or(commit))
        }
        UnprobedHead::Unknown => " HEAD unreadable".to_owned(),
    }
}

/// A reason's label; an operation outside the primary checkout names the
/// worktree it's in, and an unresolvable checkout says which kind it is.
fn needs_human_label(reason: &NeedsHuman, e: &EntryStatus, view: View<'_>) -> String {
    match reason {
        NeedsHuman::NotARepo { .. } => "not a repo".into(),
        NeedsHuman::OperationInProgress { checkout, op } => {
            let primary = e.checkouts.iter().find(|c| c.primary);
            if primary.is_some_and(|c| c.path == *checkout) {
                format!("{} in progress", op.label())
            } else {
                format!(
                    "{} in progress, worktree {}",
                    op.label(),
                    view.show(checkout)
                )
            }
        }
        NeedsHuman::OriginMismatch { origin, .. } => match origin {
            OriginRemote::Url { url } => format!("origin is {url}"),
            OriginRemote::NoUrl => "origin has no URL".into(),
            OriginRemote::Missing => "no origin".into(),
        },
        NeedsHuman::WorktreeUnreadable { path } => {
            format!("worktree git dir unreadable: {}", view.show(path))
        }
        NeedsHuman::DefaultBranchMissing { branch } => format!("no local {branch}"),
        NeedsHuman::DefaultBranchNoUpstream { branch } => {
            format!("{branch} has no origin upstream")
        }
        NeedsHuman::DefaultBranchGone { branch } => {
            format!("{branch}'s upstream is gone from origin")
        }
        NeedsHuman::UnexpectedDetached { .. } => "detached".into(),
        NeedsHuman::CheckoutUnresolvable {
            checkout,
            path,
            error,
        } => {
            let at = if path == checkout {
                String::new()
            } else {
                format!(" at {}", view.show(path))
            };
            let primary = e.checkouts.iter().find(|c| c.primary);
            let kind = if primary.is_some_and(|c| c.path == *checkout) {
                "checkout"
            } else {
                "worktree"
            };
            format!("{kind} {} unresolvable{at}: {error}", view.show(checkout))
        }
        NeedsHuman::UnlistedGitDir {
            git_dir,
            head,
            busy,
        } => format!(
            "unlisted git dir {}{} shares its refs · busy: {}",
            view.show(git_dir),
            unprobed_head_label(head),
            sessions_label(busy, view)
        ),
        NeedsHuman::CloneSharesRepo { with } => format!("same repo as {with}, not cloned"),
        NeedsHuman::ClonedUnregistered { dir } => {
            format!("already cloned as {dir}, not cloned")
        }
        NeedsHuman::OriginNotHttps {
            fetch_url,
            expected,
            ..
        } => {
            if fetches_elsewhere(fetch_url) {
                format!("refresh would fetch from {fetch_url}, not {expected}")
            } else {
                format!("refresh would fetch from {fetch_url}, not over HTTPS")
            }
        }
        NeedsHuman::FetchUrlMismatch { fetch_url, .. } => format!("fetch goes to {fetch_url}"),
        NeedsHuman::PushUrlMismatch { push_urls, .. } => match &push_urls[..] {
            [] => "push goes nowhere".into(),
            [one] => format!("push goes to {one}"),
            several => format!(
                "push goes to {} URLs: {}",
                several.len(),
                several.join(", ")
            ),
        },
    }
}

/// How to make origin's fetch reach `expected`: `fix`, the command that
/// points origin at it, or, with none, the `insteadOf` rewrite that sends
/// the fetch elsewhere, to look at. `dir` is the checkout, shell-quoted.
fn fetch_fix(fix: Option<&OriginFix>, expected: &str, dir: &str) -> String {
    let quoted = shell_quote(expected);
    match fix {
        Some(OriginFix::SetUrl) => format!("git -C {dir} remote set-url origin {quoted}"),
        Some(OriginFix::Add) => format!("git -C {dir} remote add origin {quoted}"),
        Some(OriginFix::ByHand { .. }) => format!("set remote.origin.url to {quoted} by hand"),
        None => format!(
            "a url.*.insteadOf rewrite makes it: see git -C {dir} config --get-regexp \
             '^url\\..*\\.insteadof$'"
        ),
    }
}

/// The registry, and how fresh the remote view is: the oldest fetch among
/// the owned repos sync fetches (active, not references — dormant forks
/// would pin it at months), with never-fetched ones counted apart.
fn footer(report: &StatusReport, view: View<'_>) -> String {
    let fetched: Vec<Option<u64>> = report
        .entries
        .iter()
        .filter(|e| {
            e.kind == EntryKind::Repo
                && e.writable
                && !e.archived
                && e.presence == Presence::Present
                && e.probe_error.is_none()
        })
        .map(|e| e.fetched_at)
        .collect();
    let never = fetched.iter().filter(|a| a.is_none()).count();
    let oldest = fetched.iter().flatten().min();
    let freshness = match (oldest, never) {
        (None, 0) => None,
        (None, _) => Some("never fetched".to_owned()),
        (Some(at), 0) => Some(format!("fetched {} ago", view.age(*at))),
        (Some(at), n) => Some(format!("fetched {} ago, {n} never", view.age(*at))),
    };
    let registry = view.show(&report.registry);
    freshness.map_or_else(|| registry.clone(), |f| format!("{registry} · {f}"))
}

/// `--verbose`'s block for one entry, to check a classification against git
/// by eye; `workspace` is the report's root, which entry dirs resolve
/// against.
pub fn render_entry(e: &EntryStatus, workspace: &Path, view: View<'_>) -> String {
    let mut out = String::new();
    let mut tags = vec![
        match e.kind {
            EntryKind::Repo => "repo".to_owned(),
            EntryKind::Reference => "reference".to_owned(),
        },
        if e.writable { "owned" } else { "third-party" }.to_owned(),
    ];
    if let Some(v) = e.visibility {
        tags.push(
            match v {
                Visibility::Public => "public",
                Visibility::Private => "private",
            }
            .to_owned(),
        );
    }
    if e.ci {
        tags.push("ci".into());
    }
    if e.archived {
        tags.push("archived".into());
    }
    // where the checkout lives, and who moves its HEAD
    tags.push(match (&e.branch, e.pinned) {
        (Some(branch), false) => format!("follow {branch}"),
        (None, false) => "leave HEAD".into(),
        (Some(branch), true) => format!("pinned · branch {branch}"),
        (None, true) => "pinned".into(),
    });
    match e.refresh {
        Some(RefreshVerdict::Act) => tags.push("refresh".into()),
        Some(RefreshVerdict::Held { by }) => {
            tags.push(format!(
                "refresh held{}",
                refresh_held_note(by, &e.needs_human)
            ));
        }
        None => {}
    }
    let _ = writeln!(out, "{}  {}", e.key, tags.join(" · "));
    let _ = writeln!(out, "  {:<10}{}", "url", e.url);

    match e.presence {
        Presence::Missing => {
            let _ = writeln!(out, "  {:<10}missing: {}", "dir", e.dir);
            if let Some(verdict) = &e.clone {
                let _ = writeln!(out, "  {:<10}{}", "clone", clone_label(verdict));
            }
        }
        Presence::NotARepo => {
            let _ = writeln!(out, "  {:<10}not a repo: {}", "dir", e.dir);
        }
        Presence::Present => {
            let mut facts = vec![e.fetched_at.map_or_else(
                || "never fetched".to_owned(),
                |at| format!("fetched {} ago", view.age(at)),
            )];
            if e.stashes > 0 {
                facts.push(format!("stashes {}", e.stashes));
            }
            if let Some(layout) = &e.layout {
                if layout.shallow {
                    facts.push("shallow".into());
                }
                if layout.sparse {
                    facts.push("sparse".into());
                }
                if let Some(filter) = &layout.partial_filter {
                    facts.push(format!("filter {filter}"));
                }
            }
            let _ = writeln!(out, "  {:<10}{}", "state", facts.join(" · "));
        }
    }
    for c in &e.checkouts {
        let head = match &c.head {
            Head::Branch { name } => format!("on {name}"),
            Head::Detached { commit } => {
                format!("detached at {}", commit.get(..12).unwrap_or(commit))
            }
        };
        let dirt = if c.uncommitted.is_clean() {
            "clean".to_owned()
        } else {
            uncommitted_detail(&c.uncommitted)
        };
        let op = c
            .in_progress
            .map(|op| format!(" · {} in progress", op.label()))
            .unwrap_or_default();
        let mark = match (c.primary, c.linked, c.locked) {
            (true, _, false) => "",
            (true, _, true) => " (locked)",
            (false, false, _) => " (main worktree)",
            (false, true, false) => " (worktree)",
            (false, true, true) => " (worktree, locked)",
        };
        let busy = if c.busy.is_empty() {
            String::new()
        } else {
            format!(" · busy: {}", sessions_label(&c.busy, view))
        };
        let _ = writeln!(
            out,
            "  {:<10}{}{mark} {head} · {dirt}{op}{busy}",
            "checkout",
            view.show(&c.path)
        );
    }
    for status in &e.unprobed_worktrees {
        let u = &status.worktree;
        let why = match u.why {
            UnprobedWhy::Prunable => "prunable",
            UnprobedWhy::Missing => "missing",
            UnprobedWhy::Failed { .. } => "probe failed",
        };
        let locked = if u.locked { ", locked" } else { "" };
        let head = unprobed_head_label(&u.head);
        let op = u
            .in_progress
            .map(|op| format!(" · {} in progress", op.label()))
            .unwrap_or_default();
        let busy = if status.busy.is_empty() {
            String::new()
        } else {
            format!(" · busy: {}", sessions_label(&status.busy, view))
        };
        let _ = writeln!(
            out,
            "  {:<10}{} (worktree, {why}{locked}){head}{op}{busy}",
            "checkout",
            view.show(&u.path)
        );
    }
    let name_width = e.branches.iter().map(|b| b.name.len()).max().unwrap_or(0);
    let upstream_width = e
        .branches
        .iter()
        .map(|b| b.upstream.as_deref().map_or(1, str::len))
        .max()
        .unwrap_or(0);
    for b in &e.branches {
        let mut detail = relation_label(b.relation);
        if let Some(target) = &b.symref {
            let target = target.strip_prefix("refs/heads/").unwrap_or(target);
            let _ = write!(detail, " · alias of {target}");
        }
        if b.unique_commits > 0 {
            let _ = write!(detail, " · {} unique", b.unique_commits);
        }
        let _ = write!(detail, " · {}", view.age(b.newest_commit_at));
        if b.worktree.is_some() {
            detail.push_str(" · checked out");
        }
        if let Some(verdict) = verdict_label(&b.verdict) {
            let _ = write!(detail, " → {verdict}");
        }
        let _ = writeln!(
            out,
            "  {:<10}{:<name_width$}  {:<upstream_width$}  {detail}",
            "branch",
            b.name,
            b.upstream.as_deref().unwrap_or("-"),
        );
    }
    // the checkout as a word in the commands below
    let dir = e.checkouts.first().map_or_else(
        || view.show_arg(&workspace.join(&e.dir).to_string_lossy()),
        |c| view.show_arg(&c.path),
    );
    for reason in &e.needs_human {
        let detail = match reason {
            NeedsHuman::NotARepo { detail } => format!("not a repo: {detail}"),
            NeedsHuman::OriginMismatch { expected, fix, .. } => {
                let expected = shell_quote(expected);
                let command = match fix {
                    OriginFix::Add => format!("git -C {dir} remote add origin {expected}"),
                    OriginFix::SetUrl => format!("git -C {dir} remote set-url origin {expected}"),
                    OriginFix::ByHand { reason } => format!(
                        "set remote.origin.url to {expected} by hand: {}",
                        match reason {
                            OriginByHand::OutsideRepoFile => {
                                "a URL comes from beyond the repo's own config file (global, \
                                 system, included, or worktree config)"
                            }
                            OriginByHand::EmptyValue => {
                                "an empty url among several resets the list, and git remote \
                                 set-url can't choose among several"
                            }
                            OriginByHand::ValuelessUrl => {
                                "a url with no value breaks every git remote command"
                            }
                            OriginByHand::SeveralUrls => {
                                "it has several URLs, which git remote set-url can't choose among"
                            }
                        }
                    ),
                };
                format!("{} — {command}", needs_human_label(reason, e, view))
            }
            NeedsHuman::PushUrlMismatch { expected, .. } => format!(
                "{} — sync pushes only to {expected}: see git -C {dir} remote get-url --push \
                 --all origin (remote.origin.pushurl, url.*.pushInsteadOf)",
                needs_human_label(reason, e, view)
            ),
            NeedsHuman::OriginNotHttps { expected, fix, .. } => format!(
                "{} — a reference is fetched only over HTTPS, from {expected}: {}",
                needs_human_label(reason, e, view),
                fetch_fix(fix.as_ref(), expected, &dir)
            ),
            NeedsHuman::FetchUrlMismatch { expected, fix, .. } => {
                let partial = e
                    .layout
                    .as_ref()
                    .is_some_and(|l| l.partial_filter.is_some());
                let over = if partial {
                    ", over SSH or HTTPS in a partial clone"
                } else {
                    ""
                };
                format!(
                    "{} — sync fetches only the registry's repo, {expected}{over}: {}",
                    needs_human_label(reason, e, view),
                    fetch_fix(fix.as_ref(), expected, &dir)
                )
            }
            NeedsHuman::CloneSharesRepo { with } => format!(
                "{} — sync never makes a second copy of a repo: clone {dir} by hand, or add it \
                 as a worktree of {with}",
                needs_human_label(reason, e, view)
            ),
            NeedsHuman::ClonedUnregistered { dir: at } => format!(
                "{} — sync never makes a second copy of a repo: rename {} to {dir}, or set the \
                 entry's dir to {}",
                needs_human_label(reason, e, view),
                view.show_arg(&workspace.join(at).to_string_lossy()),
                shell_quote(at)
            ),
            reason => needs_human_label(reason, e, view),
        };
        let _ = writeln!(out, "  {:<10}{detail}", "needs");
    }
    if let Some(error) = &e.probe_error {
        let _ = writeln!(out, "  {:<10}probe: {error}", "error");
        if e.probe_failed_partial() {
            let _ = writeln!(out, "  {:<10}{}", "hint", partial_hint(&dir));
        }
    }
    if let Some(failure) = &e.fetch_error {
        let _ = writeln!(
            out,
            "  {:<10}fetch: {}",
            "error",
            remote_failure_label(failure, true)
        );
        let hint = match failure {
            RemoteFailure::RefGone { fix, .. } => Some(ref_gone_hint(fix, &dir)),
            f => match unreachable_cause(f) {
                Some(UnreachableCause::HostKey) => Some(HOST_KEY_HINT.to_owned()),
                Some(UnreachableCause::Auth) => Some(AUTH_HINT.to_owned()),
                _ => None,
            },
        };
        if let Some(hint) = hint {
            let _ = writeln!(out, "  {:<10}{hint}", "hint");
        }
    }
    match &e.visibility_check {
        Some(VisibilityCheck::Leak) => {
            let _ = writeln!(
                out,
                "  {:<10}anonymously readable, though declared private",
                "access"
            );
        }
        Some(VisibilityCheck::Private) => {
            let _ = writeln!(
                out,
                "  {:<10}private as declared (anonymous read refused)",
                "access"
            );
        }
        Some(VisibilityCheck::Unknown { failure }) => {
            let _ = writeln!(
                out,
                "  {:<10}visibility check: {}",
                "error",
                remote_failure_label(failure, true)
            );
            if unreachable_cause(failure) == Some(UnreachableCause::HostKey) {
                let _ = writeln!(out, "  {:<10}{CERTIFICATE_HINT}", "hint");
            }
        }
        None => {}
    }
    for u in e.unprobed_worktrees.iter().map(|u| &u.worktree) {
        if let UnprobedWhy::Failed { error } = &u.why {
            let _ = writeln!(
                out,
                "  {:<10}worktree {}: {error}",
                "error",
                view.show(&u.path)
            );
        }
    }
    out
}

/// The summary's `unregistered` runs — `owned: …`, `third-party: …`,
/// `no origin: …` — each stray by dir name, a worktree marked with what it is
/// and, when moved, its fix. Nothing when the scan didn't run or found none.
fn unregistered_groups(report: &StatusReport, view: View<'_>) -> Vec<Vec<String>> {
    let mut owned = Vec::new();
    let mut third_party = Vec::new();
    let mut no_origin = Vec::new();
    for u in report.unregistered.iter().flatten() {
        let label = match &u.kind {
            UnregisteredKind::Clone => u.dir.clone(),
            UnregisteredKind::Worktree => format!("{} (worktree)", u.dir),
            UnregisteredKind::MovedWorktree {
                entry,
                blocked_by: None,
                ..
            } => format!(
                "{} (moved worktree of {entry} — git worktree repair)",
                u.dir
            ),
            UnregisteredKind::MovedWorktree {
                entry,
                blocked_by: Some(block),
                ..
            } => format!(
                "{} (moved worktree of {entry} — {})",
                u.dir,
                repair_block_summary(block, view)
            ),
            UnregisteredKind::OrphanedWorktree { entry } => {
                format!(
                    "{} (orphaned worktree of {entry} — its git dir is lost)",
                    u.dir
                )
            }
            UnregisteredKind::SharedGitDir { entry, with } => format!(
                "{} (shares {entry}'s git dir with {} — don't repair)",
                u.dir,
                shared_with(with.as_deref(), view)
            ),
            UnregisteredKind::UnfinishedClone => {
                format!(
                    "{} (a clone repos didn't finish, or one still running — remove it once \
                     no repos sync is running)",
                    u.dir
                )
            }
        };
        match (u.owned, &u.origin) {
            (true, _) => owned.push(label),
            (false, Some(_)) => third_party.push(label),
            (false, None) => no_origin.push(label),
        }
    }
    [
        ("owned: ", owned),
        ("third-party: ", third_party),
        ("no origin: ", no_origin),
    ]
    .into_iter()
    .filter_map(|(group, items)| prefixed(group, items))
    .collect()
}

/// The checkout a `SharedGitDir` stray shares with, shown; `None` is one
/// git's record can't name — locked, or unreadable.
fn shared_with(with: Option<&str>, view: View<'_>) -> String {
    with.map_or_else(
        || "a locked or unreadable worktree".to_owned(),
        |with| view.show(with),
    )
}

/// A worktree git dir by its id, the last component of
/// `<common>/worktrees/<id>`.
fn git_dir_id(git_dir: &str) -> &str {
    Path::new(git_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(git_dir)
}

/// What blocks a moved worktree's repair, for the summary.
fn repair_block_summary(block: &RepairBlock, view: View<'_>) -> String {
    match block {
        RepairBlock::Rewrites { path, .. } => {
            format!(
                "a repair would also rewrite {}; fix that first",
                view.show(path)
            )
        }
        RepairBlock::ClaimedDir { .. } => {
            "another moved worktree claims this dir; repair that one first once its repair is \
             offered, then rerun"
                .to_owned()
        }
        RepairBlock::Swapped { with, .. } => format!("swapped with {with}; move the dirs back"),
        RepairBlock::RelativeGitdir { .. } => {
            "a relative gitdir in this repo, which git versions resolve differently; fix by hand"
                .to_owned()
        }
        RepairBlock::UnreadableGitdir { .. } => {
            "a gitdir in this repo can't be read; fix by hand".to_owned()
        }
        RepairBlock::NulInGitdir { .. } => {
            "a NUL in its git dir's gitdir, so a repair may change nothing; its fix under \
             --verbose"
                .to_owned()
        }
        RepairBlock::NonUtf8Path => {
            "its path isn't UTF-8; rename it to a UTF-8 name, then rerun".to_owned()
        }
    }
}

/// `--verbose`'s block for one unregistered dir: what it is, its origin,
/// and the fix when there's a mechanical one.
pub fn render_unregistered(u: &UnregisteredClone, report: &StatusReport, view: View<'_>) -> String {
    let workspace = Path::new(&report.workspace);
    let path = workspace.join(&u.dir);
    let path = path.to_string_lossy();
    let at = view.show(&path);
    // an entry's dir as a word in a command
    let entry_dir = |key: &str| {
        let dir = report
            .entries
            .iter()
            .find(|e| e.key == key)
            .map_or(key, |e| e.dir.as_str());
        view.show_arg(&workspace.join(dir).to_string_lossy())
    };
    let owner = match (u.owned, &u.origin) {
        (true, _) => "owned",
        (false, Some(_)) => "third-party",
        (false, None) => "no origin",
    };
    let (kind, detail) = match &u.kind {
        UnregisteredKind::Clone => ("clone".to_owned(), vec![]),
        UnregisteredKind::Worktree => (
            "worktree".to_owned(),
            vec![(
                "note",
                "a worktree git doesn't list for any registered repo, a moved worktree whose \
                 .git is a link (replace the link with its file, then rerun repos status), or a \
                 .git that can't be read"
                    .to_owned(),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: None,
            exit_noise,
        } => {
            let mut detail = vec![(
                "fix",
                format!(
                    "git -C {} worktree repair {}",
                    entry_dir(entry),
                    view.show_arg(&path)
                ),
            )];
            if let Some(path) = exit_noise {
                detail.push((
                    "note",
                    format!(
                        "git will complain about {} and exit 1, leaving it be; this one is \
                         repaired all the same",
                        view.show(path)
                    ),
                ));
            }
            (format!("moved worktree of {entry}"), detail)
        }
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::Rewrites { path, git_dir }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                format!(
                    "git worktree repair would also rewrite {}/.git — {entry}'s worktree git \
                     dir {} names it, and its .git is missing or names another — fix that first",
                    view.show(path),
                    git_dir_id(git_dir)
                ),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::Swapped { git_dir, with }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                format!(
                    "swapped by hand with {with}: {entry}'s worktree git dir {} names this dir \
                     while {with}'s .git names it — move the two dirs back; a repair of either \
                     would hijack the other",
                    git_dir_id(git_dir)
                ),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::ClaimedDir { git_dir }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                format!(
                    "{entry}'s worktree git dir {} names this dir, so a repair would point this \
                     .git there — repair the moved worktree whose .git names it first, once its \
                     repair is offered, then rerun repos status",
                    git_dir_id(git_dir)
                ),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::RelativeGitdir { git_dir }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                format!(
                    "{entry}'s worktree git dir {} names its worktree by a relative path, which \
                     git 2.48+ resolves against the git dir and older gits against the cwd — \
                     what a repair would touch is uncertain, so none is offered; make that gitdir \
                     absolute by hand, then rerun repos status",
                    git_dir_id(git_dir)
                ),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::UnreadableGitdir { git_dir }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                format!(
                    "{entry}'s worktree git dir {} has a gitdir that can't be read by this \
                     tool (unreadable, or past its size limit, which git may read fine) — what a \
                     repair would touch is unknown, so none is offered; trim or fix it by hand, \
                     then rerun repos status",
                    git_dir_id(git_dir)
                ),
            )],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::NulInGitdir { git_dir }),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![
                (
                    "note",
                    format!(
                        "{entry}'s worktree git dir {} holds a NUL in its gitdir — git lists \
                         this worktree by what's before the NUL, while a repair here compares \
                         that with this .git and may change nothing; the fix writes this .git \
                         into that gitdir",
                        git_dir_id(git_dir)
                    ),
                ),
                (
                    "fix",
                    format!(
                        "printf '%s\\n' {} > {}",
                        view.show_arg(&Path::new(&*path).join(".git").to_string_lossy()),
                        view.show_arg(&Path::new(git_dir).join("gitdir").to_string_lossy())
                    ),
                ),
            ],
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(RepairBlock::NonUtf8Path),
            ..
        } => (
            format!("moved worktree of {entry}"),
            vec![(
                "note",
                "its path isn't UTF-8, so no repair command here can name it exactly — rename \
                 it to a UTF-8 name, then rerun repos status"
                    .to_owned(),
            )],
        ),
        UnregisteredKind::OrphanedWorktree { entry } => (
            format!("orphaned worktree of {entry}"),
            vec![(
                "note",
                "its git dir is gone or holds no HEAD — its index and HEAD are lost, and git \
                 worktree repair can't reconnect it"
                    .to_owned(),
            )],
        ),
        UnregisteredKind::SharedGitDir { entry, with } => (
            format!("shares a git dir of {entry}"),
            vec![(
                "note",
                format!(
                    "{} uses or may use it: this is a copy, or an orphan whose git-dir id git \
                     reused — git worktree repair here would take the git dir from there",
                    shared_with(with.as_deref(), view)
                ),
            )],
        ),
        UnregisteredKind::UnfinishedClone => (
            "unfinished clone".to_owned(),
            vec![(
                "note",
                "a clone repos didn't finish, or one still running, in its temp dir — nothing \
                 to keep: remove it once no repos sync is running"
                    .to_owned(),
            )],
        ),
    };
    let mut out = format!("{}  unregistered · {owner} · {kind}\n", u.dir);
    let _ = writeln!(out, "  {:<10}{at}", "dir");
    let _ = writeln!(
        out,
        "  {:<10}{}",
        "origin",
        u.origin.as_deref().unwrap_or("none")
    );
    for (label, detail) in detail {
        let _ = writeln!(out, "  {label:<10}{detail}");
    }
    out
}

/// A remote URL shortened for the summary: `account/name` when it's on the
/// registry URL's host, else the URL as configured.
fn compact_remote(origin: &str, registry_url: &str) -> String {
    let host = registry_url
        .strip_prefix("https://")
        .and_then(|r| r.split('/').next())
        .unwrap_or_default();
    let path = [
        format!("git@{host}:"),
        format!("ssh://git@{host}/"),
        format!("https://{host}/"),
    ]
    .iter()
    .find_map(|prefix| origin.strip_prefix(prefix.as_str()));
    path.map_or_else(
        || origin.to_owned(),
        |p| p.trim_end_matches('/').trim_end_matches(".git").to_owned(),
    )
}

fn relation_label(r: Relation) -> String {
    match r {
        Relation::InSync => "in sync".into(),
        Relation::Ahead { commits } => format!("ahead {commits}"),
        Relation::Behind { commits } => format!("behind {commits}"),
        Relation::Diverged { ahead, behind } => format!("diverged +{ahead} −{behind}"),
        Relation::Shallow => "shallow, tips differ".into(),
        Relation::Gone => "upstream gone".into(),
        Relation::Unmapped => "outside refspec".into(),
        Relation::Untracked => "untracked".into(),
    }
}

/// The verdict for `--verbose`'s branch lines; `None` when quiet.
fn verdict_label(v: &Verdict) -> Option<String> {
    let verb = action_verb;
    Some(match v {
        Verdict::Quiet => return None,
        Verdict::Act { action } => verb(*action).to_owned(),
        Verdict::Held { action, by } => format!("held {}{}", verb(*action), held_note(*by)),
        Verdict::NeedsHuman { .. } => "needs human".to_owned(),
        Verdict::LocalOnly => "local-only".to_owned(),
        Verdict::Cleanup {
            removable_worktree: Some(_),
            ..
        } => "cleanup, worktree removable (ignored files go with it)".to_owned(),
        Verdict::Cleanup { .. } => "cleanup".to_owned(),
    })
}

/// A clone verdict as `--verbose`'s entry block words it: the recipe's
/// URL and its flags, and what holds it.
fn clone_label(verdict: &CloneVerdict) -> String {
    let recipe = verdict.recipe();
    let mut parts = vec![recipe.url.clone()];
    parts.push(recipe.branch.as_ref().map_or_else(
        || "the remote's default branch".to_owned(),
        |b| format!("branch {b}"),
    ));
    if recipe.shallow {
        parts.push("depth 1".into());
    }
    if let Some(path) = &recipe.sparse {
        parts.push(format!("sparse {path}"));
    }
    if let CloneVerdict::Held { by, .. } = verdict {
        parts.push(format!("held{}", held_note(*by)));
    }
    parts.join(" · ")
}

/// An action's verb, as the summary's runs name it.
const fn action_verb(action: SyncAction) -> &'static str {
    match action {
        SyncAction::Push { .. } => "push",
        SyncAction::FastForward { .. } => "ff",
        SyncAction::Move => "move",
    }
}

/// What a held action's label carries after it; an entry-level hold has its
/// reason printed on the entry instead.
fn held_note(by: HeldBy) -> &'static str {
    hold_note(by.into())
}

/// `held_note` for a held refresh: the one entry-level reason that holds a
/// refresh is origin drift (`refresh_verdict`), named here since the
/// entry's origin-drift line may not print (a probe that failed after
/// reading the config). An origin not over HTTPS has a note of its own,
/// worded from the entry's `origin_not_https` reason: a fetch that is over
/// HTTPS after all reaches another repo.
fn refresh_held_note(by: HeldBy, reasons: &[NeedsHuman]) -> &'static str {
    let elsewhere = || {
        reasons.iter().any(|r| {
            matches!(r, NeedsHuman::OriginNotHttps { fetch_url, .. } if fetches_elsewhere(fetch_url))
        })
    };
    match by {
        HeldBy::Entry => " (origin drift)",
        HeldBy::OriginNotHttps if elsewhere() => " (origin elsewhere)",
        by => held_note(by),
    }
}

/// Whether an `origin_not_https` reason's fetch URL is HTTPS after all —
/// an `insteadOf` rewrite naming another repo — so its wording names the
/// repo it would reach, not the transport.
fn fetches_elsewhere(fetch_url: &str) -> bool {
    fetch_url.starts_with("https://")
}

/// `held_note` for a hold sync found, the verdict's or its own.
const fn hold_note(by: SyncHold) -> &'static str {
    match by {
        SyncHold::Pinned => " (pinned)",
        SyncHold::Entry => "",
        SyncHold::PushUrl => " (push URL)",
        SyncHold::OriginNotHttps => " (origin not HTTPS)",
        SyncHold::FetchFailed => " (fetch failed)",
        SyncHold::DirtyCheckout => " (dirty)",
        SyncHold::UnprobedWorktree => " (unprobed worktree)",
        SyncHold::SeveralCheckouts => " (several checkouts)",
        SyncHold::Busy => " (busy)",
        SyncHold::BusyUnknown => " (busy unknown)",
        SyncHold::Changed => " (changed since read, rerun)",
    }
}

/// Why busy detection is unavailable, as the `failed` line words it.
fn unavailable_label(reason: &Unavailable, view: View<'_>) -> String {
    match reason {
        Unavailable::HomeUnknown => "HOME isn't set, so ~/.claude can't be found".into(),
        Unavailable::RelativeConfigDir { path } => {
            format!("config dir {path} isn't an absolute path")
        }
        Unavailable::Unreadable { path, error } => {
            format!("can't read {}: {error}", view.show(path))
        }
        Unavailable::Unparseable { path, error } => {
            format!("can't parse {}: {error}", view.show(path))
        }
        Unavailable::ForeignPidDomain {
            path,
            pid_domain,
            source,
        } => {
            // a session file names one session; the roster isn't one to remove
            let hint = match source {
                SessionSource::SessionFile => " — remove it if that session is gone",
                SessionSource::RosterWorker => "",
            };
            format!(
                "{} is from another machine or pid namespace ({pid_domain}){hint}",
                view.show(path)
            )
        }
    }
}

/// A session as `--verbose` lists it: pid, and cwd — and its worktree and
/// its process's cwd, when it has them.
fn session_label(s: &Session, view: View<'_>) -> String {
    let mut label = format!("pid {} ({}", s.pid, view.show(&s.cwd));
    if let Some(worktree) = &s.worktree {
        let _ = write!(label, ", worktree {}", view.show(worktree));
    }
    if let Some(now) = &s.process_cwd {
        let _ = write!(label, ", now {}", view.show(now));
    }
    label.push(')');
    label
}

/// A checkout's sessions, for `--verbose`'s entry block.
fn sessions_label(sessions: &[Session], view: View<'_>) -> String {
    sessions
        .iter()
        .map(|s| session_label(s, view))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What removing a gone worktree would discard, as the cleanup line words
/// it.
fn prune_loss_label(loss: &PruneLoss) -> String {
    match loss {
        PruneLoss::Operation { op } => format!("its {} in progress", op.label()),
        PruneLoss::DetachedHead => "its detached HEAD".into(),
        PruneLoss::UnknownHead => "its HEAD".into(),
        PruneLoss::MissingBranch { name } => format!("its HEAD (branch {name} is gone)"),
        PruneLoss::Submodules => "its submodules' repos".into(),
        PruneLoss::WorktreeRefs => "its worktree refs".into(),
        PruneLoss::StagedChanges => "its staged changes".into(),
        PruneLoss::UnmatchedGitDir => "whatever its git dir holds (it can't be matched)".into(),
        PruneLoss::RelativeGitdir { git_dir } => format!(
            "its index and HEAD if it isn't gone after all (git dir {} names its worktree \
             relatively, which git versions resolve differently)",
            git_dir_id(git_dir)
        ),
    }
}

/// An entry's dirt as one item: the primary's total, then another dirty
/// worktree by its shown path, or several folded into a count with their
/// summed total. `None` when every checkout is clean.
fn uncommitted_summary(key: &str, checkouts: &[Checkout], view: View<'_>) -> Option<String> {
    let total = |c: &Checkout| u64::from(c.uncommitted.total());
    let primary = checkouts
        .iter()
        .filter(|c| c.primary)
        .map(total)
        .sum::<u64>();
    let others = checkouts
        .iter()
        .filter(|c| !c.primary && !c.uncommitted.is_clean())
        .collect::<Vec<_>>();
    let more = others.iter().map(|c| total(c)).sum::<u64>();
    let detail = match (primary, others.as_slice()) {
        (0, []) => return None,
        (n, []) => group_digits(n),
        (0, [c]) => format!("worktree {}, {}", view.show(&c.path), group_digits(more)),
        (n, [c]) => format!(
            "{}; worktree {}, {} more",
            group_digits(n),
            view.show(&c.path),
            group_digits(more)
        ),
        (n, many) => format!(
            "{}; {} worktrees, {} more",
            if n == 0 {
                "clean".to_owned()
            } else {
                group_digits(n)
            },
            group_digits(many.len() as u64),
            group_digits(more)
        ),
    };
    Some(format!("{key} ({detail})"))
}

/// A count with its digits in groups of three, split by commas: `1,040`.
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

fn uncommitted_detail(u: &Uncommitted) -> String {
    [
        (u.staged, "staged"),
        (u.unstaged, "unstaged"),
        (u.untracked, "untracked"),
        (u.conflicted, "conflicted"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// A remote failure in words: the kind, and under `detail` the line git
/// printed that decided it.
fn remote_failure_label(f: &RemoteFailure, detail: bool) -> String {
    let with = |words: &str, message: &str| {
        if detail {
            format!("{words} — {message}")
        } else {
            words.to_owned()
        }
    };
    match f {
        RemoteFailure::RefGone { refname, .. } => format!("origin has no {refname}"),
        RemoteFailure::Unreachable { cause, message } => with(
            match cause {
                UnreachableCause::Dns => "host not found",
                UnreachableCause::Connection => "no connection",
                UnreachableCause::HostKey => "host not trusted",
                UnreachableCause::Auth => "access denied",
            },
            message,
        ),
        RemoteFailure::RepoNotFound { message } => with("repo not found", message),
        RemoteFailure::TimedOut { after_secs } => format!("timed out after {after_secs}s"),
        RemoteFailure::Failed { message } => first_line(message).to_owned(),
        RemoteFailure::Rejected { reason, message } => match message {
            Some(message) if detail => format!("rejected ({reason}) — {message}"),
            Some(message) => format!("rejected: {message}"),
            None => format!("rejected ({reason})"),
        },
        RemoteFailure::RefspecOutsideOrigin { refspec } => {
            format!("not run — refspec {refspec} writes outside refs/remotes/origin/")
        }
        RemoteFailure::LegacyRemotesUnreadable { path } => format!(
            "not run — the legacy remote {path} couldn't be read, and may share origin's refs"
        ),
        RemoteFailure::OriginRefsShared { remote, refspec } => format!(
            "not run — remote {remote}'s refspec {refspec} can write under \
             refs/remotes/origin/, which pruning origin may empty"
        ),
    }
}

/// Why an entry's visibility check couldn't reach its host; `None` when it
/// did, or didn't run.
const fn visibility_cause(e: &EntryStatus) -> Option<UnreachableCause> {
    match &e.visibility_check {
        Some(VisibilityCheck::Unknown { failure }) => unreachable_cause(failure),
        _ => None,
    }
}

/// An unreachable host's cause; `None` for any other failure.
const fn unreachable_cause(f: &RemoteFailure) -> Option<UnreachableCause> {
    match f {
        RemoteFailure::Unreachable { cause, .. } => Some(*cause),
        _ => None,
    }
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim()
}

/// A compact age: `45s`, `12m`, `3h`, `5d`, `4mo`, `2y`.
fn format_age(secs: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    match secs {
        s if s < MIN => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MIN),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < 60 * DAY => format!("{}d", s / DAY),
        s if s < 365 * DAY => format!("{}mo", s / (30 * DAY)),
        s => format!("{}y", s / (365 * DAY)),
    }
}

#[cfg(test)]
mod tests {
    use fuz_repos::registry::{EntryKind, Visibility};
    use fuz_repos::report::{BranchSync, FetchOutcome};
    use fuz_repos::state::{
        AtRest, Checkout, CloneRecipe, InProgressOp, Layout, UnprobedWorktree,
        UnprobedWorktreeStatus,
    };

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
        let untracked = |name: &str, upstream| {
            branch(name, upstream, Relation::Untracked, 1, Verdict::LocalOnly)
        };
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
            block.starts_with(
                "wpt  reference · owned · pinned · branch fork · refresh held (pinned)\n"
            ),
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
        assert!(
            block("leaky").contains("  access    anonymously readable, though declared private\n")
        );
        assert!(
            block("sealed").contains("  access    private as declared (anonymous read refused)\n")
        );
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
            Some(
                "repos: app — rebase in progress; diverged from origin/main +2 −1 (fetched 3h ago)\n"
            )
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
}
