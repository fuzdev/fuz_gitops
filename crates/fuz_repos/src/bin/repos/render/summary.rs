//! The grouped summaries of a status, sync, and push report.

use super::*;

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
    let mut g = PushGroups::default();
    // the guard itself failed: said once, first among the failures
    if let Sessions::Unavailable { reason } = &report.status.sessions {
        g.problems.failed.push(format!(
            "busy detection ({}; every push held)",
            unavailable_label(reason, view)
        ));
    }
    for e in &report.status.entries {
        g.problems.add_entry(e, view);
    }
    for p in &report.pushes {
        g.add(p, report, view);
    }
    g.render(report, view)
}

/// A hint a push's branches call for, said once however many do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushHint {
    Diverged,
    Unmapped,
    Exists,
    Behind,
    NewBranch,
    Merged,
    DefaultGone,
    OtherUpstream,
}

impl PushHint {
    /// The hint, as printed after `hint: `.
    const fn text(self) -> &'static str {
        match self {
            Self::Diverged => DIVERGED_HINT,
            Self::Unmapped => UNMAPPED_HINT,
            Self::Exists => EXISTS_HINT,
            Self::Behind => BEHIND_HINT,
            Self::NewBranch => NEW_BRANCH_HINT,
            Self::Merged => MERGED_HINT,
            Self::DefaultGone => DEFAULT_GONE_HINT,
            Self::OtherUpstream => OTHER_UPSTREAM_HINT,
        }
    }

    /// The hint for a branch with no upstream on origin, by what
    /// `--new-branch` would do with it.
    const fn no_upstream(why: NoUpstreamWhy) -> Self {
        match why {
            NoUpstreamWhy::Creatable => Self::NewBranch,
            NoUpstreamWhy::Merged => Self::Merged,
            NoUpstreamWhy::DefaultGone => Self::DefaultGone,
            NoUpstreamWhy::OtherUpstream => Self::OtherUpstream,
        }
    }
}

/// The hints a push's branches call for after the `needs human` group, in
/// the order they print.
const PUSH_NEEDS_HUMAN_HINTS: [PushHint; 3] =
    [PushHint::Diverged, PushHint::Unmapped, PushHint::Exists];

/// The hints a push's branches call for after the `not pushed` group, in
/// the order they print.
const PUSH_NOT_PUSHED_HINTS: [PushHint; 5] = [
    PushHint::Behind,
    PushHint::NewBranch,
    PushHint::Merged,
    PushHint::DefaultGone,
    PushHint::OtherUpstream,
];

/// `repos push`'s groups, gathered before any prints.
#[derive(Debug, Default)]
struct PushGroups {
    problems: Problems,
    pushed: Vec<String>,
    in_sync: Vec<String>,
    held: Vec<String>,
    not_pushed: Vec<String>,
    /// The branch hints called for, as many times as branches call for
    /// them: each prints once, in its group's order
    /// (`PUSH_NEEDS_HUMAN_HINTS`, `PUSH_NOT_PUSHED_HINTS`).
    hints: Vec<PushHint>,
}

impl PushGroups {
    /// Adds one target's push, labeled by its entry's key and, when it isn't
    /// the registry's branch, the branch.
    fn add(&mut self, p: &CheckoutPush, report: &PushReport, view: View<'_>) {
        let Some(e) = report.status.entries.iter().find(|e| e.key == p.key) else {
            self.problems
                .failed
                .push(format!("{} (push: no status)", p.key));
            return;
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
            PushOutcome::Pushed { .. } => self.pushed.push(format!("{label}{ahead}")),
            PushOutcome::Created { .. } => self.pushed.push(format!("{label} (new branch)")),
            PushOutcome::RemoteBranchExists { at } => {
                self.hints.push(PushHint::Exists);
                self.problems.needs_human.push(format!(
                    "{label} (on origin already, at {})",
                    at.get(..7).unwrap_or(at)
                ));
            }
            PushOutcome::InSync => self.in_sync.push(label),
            PushOutcome::Held { by } => {
                self.held.push(format!("{label}{ahead}{}", hold_note(*by)));
            }
            PushOutcome::PushFailed { failure } => {
                self.problems
                    .failed
                    .push(format!("{label} (push: {})", failure.words(false)));
            }
            PushOutcome::Failed { message } => {
                self.problems
                    .failed
                    .push(format!("{label} (push: {})", first_line(message)));
            }
            PushOutcome::NotAhead => {
                self.hints.push(PushHint::Behind);
                let relation =
                    b.map_or_else(|| "not ahead".to_owned(), |b| relation_label(b.relation));
                self.not_pushed.push(format!("{label} ({relation})"));
            }
            PushOutcome::NeedsHuman { reason } => {
                match reason {
                    BranchNeedsHuman::Diverged => self.hints.push(PushHint::Diverged),
                    BranchNeedsHuman::Unmapped => self.hints.push(PushHint::Unmapped),
                    _ => {}
                }
                let why = b.map_or_else(
                    || format!("{reason:?}"),
                    |b| branch_needs_human_label(*reason, b),
                );
                self.problems.needs_human.push(format!("{label} ({why})"));
            }
            PushOutcome::NoUpstream { why } => {
                self.hints.push(PushHint::no_upstream(*why));
                self.not_pushed
                    .push(format!("{label} ({})", no_upstream_label(*why, b)));
            }
            PushOutcome::Detached => self.not_pushed.push(format!("{label} (detached HEAD{at})")),
            PushOutcome::Unread => {
                self.not_pushed
                    .push(format!("{label} ({}{at})", unread_why(e)));
            }
        }
    }

    /// The groups in order, each followed by the hints it calls for, then
    /// the registry line.
    fn render(self, report: &PushReport, view: View<'_>) -> String {
        let mut out = String::new();
        let mut line = |label: &str, tone: Tone, items: Vec<String>| {
            out.push_str(&render_group(label, tone, &Items::Singles(items), view));
        };
        let hint = |s: &str| vec![format!("hint: {s}")];
        line("visibility", Tone::Red, self.problems.visibility);
        line("failed", Tone::Red, self.problems.failed);
        for remote_hint in push_remote_hints(report) {
            line("", Tone::Plain, hint(remote_hint));
        }
        line("needs human", Tone::Red, self.problems.needs_human);
        for branch_hint in PUSH_NEEDS_HUMAN_HINTS {
            if self.hints.contains(&branch_hint) {
                line("", Tone::Plain, hint(branch_hint.text()));
            }
        }
        line("origin drift", Tone::Yellow, self.problems.origin_drift);
        line("pushed", Tone::Green, self.pushed);
        line("in sync", Tone::Plain, self.in_sync);
        line("held", Tone::Yellow, self.held);
        line("not pushed", Tone::Yellow, self.not_pushed);
        for branch_hint in PUSH_NOT_PUSHED_HINTS {
            if self.hints.contains(&branch_hint) {
                line("", Tone::Plain, hint(branch_hint.text()));
            }
        }
        let _ = writeln!(out, "{}", footer(&report.status, view));
        out
    }
}

/// The hints a fetch's failure or a push's calls for — a gone ref, an
/// untrusted host, refused credentials — each once, in that order.
fn push_remote_hints(report: &PushReport) -> Vec<&'static str> {
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
    let mut hints = Vec::new();
    if remote_failed(|f| matches!(f, RemoteFailure::RefGone { .. })) {
        hints.push(PUSH_REF_GONE_HINT);
    }
    if remote_failed(|f| unreachable_cause(f) == Some(UnreachableCause::HostKey)) {
        hints.push(HOST_KEY_HINT);
    }
    if remote_failed(|f| unreachable_cause(f) == Some(UnreachableCause::Auth)) {
        hints.push(AUTH_HINT);
    }
    hints
}

/// Why a push target's checkout wasn't read.
const fn unread_why(e: &EntryStatus) -> &'static str {
    match e.presence {
        Presence::Missing => "missing",
        Presence::NotARepo => "not a repo",
        Presence::Present if e.probe_error.is_some() => "probe failed",
        Presence::Present => "checkout not read",
    }
}

/// Why a branch with no upstream on origin wasn't pushed, worded from why
/// (the push's reading) and, where that leaves it open, the branch's
/// relation and upstream.
fn no_upstream_label(why: NoUpstreamWhy, b: Option<&BranchStatus>) -> String {
    match (why, b.map(|b| (b.relation, b.upstream.as_deref()))) {
        (NoUpstreamWhy::DefaultGone, _) => {
            "the entry's branch, upstream gone from origin".to_owned()
        }
        (NoUpstreamWhy::Merged, _) => "nothing unique, upstream gone from origin".to_owned(),
        (_, Some((Relation::Gone, _))) => "upstream gone from origin".to_owned(),
        (_, Some((Relation::Untracked, Some(upstream)))) => format!("tracks {upstream}"),
        _ => "no upstream on origin".to_owned(),
    }
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
        if !g.add(e, sync, workspace, view, verbose) {
            quiet.add(e);
        }
    }

    // the guard itself failed: said once, first among the failures
    if let Sessions::Unavailable { reason } = &report.sessions {
        g.problems.failed.insert(
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
    let hint = |hint: String| Items::Singles(vec![format!("hint: {hint}")]);
    line(
        "visibility",
        Tone::Red,
        Items::Singles(g.problems.visibility),
    );
    line("failed", Tone::Red, Items::Singles(g.problems.failed));
    for failure_hint in failure_hints(report, synced) {
        line("", Tone::Plain, hint(failure_hint));
    }
    line(
        "needs human",
        Tone::Red,
        Items::Singles(g.problems.needs_human),
    );
    line(
        "origin drift",
        Tone::Yellow,
        Items::Singles(g.problems.origin_drift),
    );
    if let Some(fix) = origin_fix_hint(report) {
        line("", Tone::Plain, hint(fix));
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

    let _ = writeln!(out, "{}      {}", quiet.line(), footer(report, view));
    out
}

/// The hints the failures call for, in the order they print: a partial
/// clone's failed probe, then a gone ref, an untrusted host, and refused
/// credentials — on a fetch, or under sync a push or a clone — then a
/// certificate the visibility check couldn't verify.
fn failure_hints(report: &StatusReport, synced: Option<&[EntrySync]>) -> Vec<String> {
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
    hints
}

/// The hint for the fixes the origin drifts call for, each worded once;
/// `None` when there's no drift.
fn origin_fix_hint(report: &StatusReport) -> Option<String> {
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
    (!fixes.is_empty()).then(|| format!("{} (each under --verbose)", fixes.join(", or ")))
}

/// The quiet entries, counted.
#[derive(Debug, Default)]
struct Counts {
    clean: u32,
    on_branches: u32,
    pinned: u32,
}

impl Counts {
    /// Counts a quiet entry: pinned, on another branch than the one it
    /// follows, or clean.
    fn add(&mut self, e: &EntryStatus) {
        // a quiet entry off its followed branch is on another one: detached
        // off it is an `unexpected_detached` reason, or its operation's
        match (e.pinned, e.at_rest.and_then(|r| r.on_branch)) {
            (true, _) => self.pinned += 1,
            (false, Some(false)) => self.on_branches += 1,
            (false, Some(true) | None) => self.clean += 1,
        }
    }

    /// The counts, as the footer's line leads with them.
    fn line(&self) -> String {
        format!(
            "clean {} · on branches {} · pinned {}",
            self.clean, self.on_branches, self.pinned
        )
    }
}

/// A summary group's items.
#[derive(Debug)]
pub(super) enum Items {
    /// Items that stand apart, two spaces between them.
    Singles(Vec<String>),
    /// Runs of items — a verb's branches, an ownership's strays — each run's
    /// items joined by `, `, the runs by the separator.
    Runs(Vec<Vec<String>>, &'static str),
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
pub(super) fn render_group(label: &str, tone: Tone, items: &Items, view: View<'_>) -> String {
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

/// The groups an entry's own failures and reasons land in, the status
/// summary's and push's alike.
#[derive(Debug, Default)]
struct Problems {
    visibility: Vec<String>,
    failed: Vec<String>,
    needs_human: Vec<String>,
    origin_drift: Vec<String>,
}

impl Problems {
    /// Adds `e`'s failed probe and fetch, its visibility check's finding,
    /// and its needs-human reasons — an origin mismatch as drift, with what
    /// origin was.
    fn add_entry(&mut self, e: &EntryStatus, view: View<'_>) {
        let key = &e.key;
        if let Some(error) = &e.probe_error {
            self.failed
                .push(format!("{key} (probe: {})", first_line(&error.message)));
        }
        if let Some(failure) = &e.fetch_error {
            self.failed
                .push(format!("{key} (fetch: {})", failure.words(false)));
        }
        match &e.visibility_check {
            Some(VisibilityCheck::Leak) => self
                .visibility
                .push(format!("{key} (declared private, anonymously readable)")),
            Some(VisibilityCheck::Unknown { failure }) => self.failed.push(format!(
                "{key} (visibility check: {})",
                failure.words(false)
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
    }

    const fn len(&self) -> usize {
        self.visibility.len() + self.failed.len() + self.needs_human.len() + self.origin_drift.len()
    }
}

#[derive(Debug, Default)]
struct Groups {
    /// A sync report's: `act` is what sync did.
    synced: bool,
    problems: Problems,
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
        self.problems.add_entry(e, view);
        self.add_refresh_item(e, sync);
        self.add_clone_item(e, sync);
        self.add_branch_items(e, sync, view);
        self.add_unprobed_items(e, workspace, view);
        self.add_uncommitted_items(e, view, verbose);
        let said = self.len() > before;
        if verbose && e.stashes > 0 {
            self.stashes.push(format!("{} ({})", e.key, e.stashes));
        }
        said
    }

    /// A reference asked for by name or `--references`: under sync, a
    /// refresh is its fetch (a failed one is said by `Problems`, as failed).
    fn add_refresh_item(&mut self, e: &EntryStatus, sync: Option<&EntrySync>) {
        match (&e.refresh, sync.map(|s| &s.fetch)) {
            (Some(RefreshVerdict::Act), None | Some(FetchOutcome::Fetched)) => {
                self.act.add_refresh(&e.key, "");
            }
            (Some(RefreshVerdict::Held { by }), _) => {
                self.held
                    .add_refresh(&e.key, refresh_held_note(*by, &e.needs_human));
            }
            (Some(RefreshVerdict::Act), Some(_)) | (None, _) => {}
        }
    }

    /// A missing entry's clone: its verdict, or under sync what sync did.
    fn add_clone_item(&mut self, e: &EntryStatus, sync: Option<&EntrySync>) {
        let key = &e.key;
        match (&e.clone, self.synced) {
            (Some(_), true) => {
                self.add_clone_outcome(key, sync.and_then(|s| s.clone.as_ref()));
            }
            (Some(CloneVerdict::Act { .. }), false) => self.act.add_clone(key, ""),
            (Some(CloneVerdict::Held { by, .. }), false) => {
                self.held.add_clone(key, clone_held_note(*by));
            }
            (None, _) => {}
        }
    }

    /// Each branch's verdict, or under sync, for one sync would act on,
    /// what it did.
    fn add_branch_items(&mut self, e: &EntryStatus, sync: Option<&EntrySync>, view: View<'_>) {
        for (bi, b) in e.branches.iter().enumerate() {
            if self.synced && matches!(b.verdict, Verdict::Act { .. } | Verdict::Held { .. }) {
                let synced = sync.and_then(|s| s.branches.get(bi));
                // another entry sharing the repo says it, once
                if synced.is_some_and(|s| s.repeats.is_some()) {
                    continue;
                }
                self.add_outcome(b, synced.map(|s| &s.outcome), &branch_label(e, b));
                continue;
            }
            self.add_verdict(e, b, view);
        }
    }

    /// A branch's verdict, in the group it puts the branch in.
    fn add_verdict(&mut self, e: &EntryStatus, b: &BranchStatus, view: View<'_>) {
        match &b.verdict {
            // a pin is the consumer's standing choice, not a hold to clear:
            // the entry counts as pinned, and `--verbose` shows what it holds
            Verdict::Quiet
            | Verdict::Held {
                by: BranchHold::Pinned,
                ..
            } => {}
            Verdict::Act { action } => self.act.add(*action, &branch_label(e, b), ""),
            Verdict::Held { action, by } => {
                self.held.add(*action, &branch_label(e, b), held_note(*by));
            }
            Verdict::NeedsHuman { reason } => {
                self.problems.needs_human.push(format!(
                    "{} ({})",
                    branch_label(e, b),
                    branch_needs_human_label(*reason, b)
                ));
            }
            Verdict::LocalOnly => {
                let read_only = if e.writable { "" } else { ", read-only" };
                self.local_only.push(format!(
                    "{} (+{}, {}{read_only})",
                    branch_label(e, b),
                    b.unique_commits,
                    view.age(b.newest_commit_at)
                ));
            }
            Verdict::Cleanup {
                reason,
                removable_worktree,
            } => {
                let why = cleanup_why(*reason, removable_worktree.as_deref(), b, view);
                self.cleanup.push(format!("{} ({why})", branch_label(e, b)));
            }
        }
    }

    /// The entry's unprobed worktrees: a failed probe as failed (but one
    /// a needs-human reason already says, `EntryStatus::unprobed_failures`),
    /// a gone one as cleanup.
    fn add_unprobed_items(&mut self, e: &EntryStatus, workspace: &Path, view: View<'_>) {
        for (u, error) in e.unprobed_failures() {
            self.problems.failed.push(format!(
                "{} (worktree {}: {})",
                e.key,
                view.show(&u.worktree.path),
                first_line(error)
            ));
        }
        // a missing one is intentional, as on unmounted media: `--verbose`
        // shows it, and it still holds its branch's fast-forward and move
        // (its push too, when a session works in its files wherever they're
        // mounted)
        for u in &e.unprobed_worktrees {
            if u.worktree.why == UnprobedWhy::Prunable {
                self.cleanup.push(prunable_item(e, u, workspace, view));
            }
        }
    }

    /// The entry's dirty checkouts: under `--verbose` each its own item,
    /// another worktree by its shown path beside the primary's key; else one
    /// summary item.
    fn add_uncommitted_items(&mut self, e: &EntryStatus, view: View<'_>, verbose: bool) {
        let key = &e.key;
        if verbose {
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
    }

    /// A missing entry's clone, as what sync did: `outcome` is `None` when
    /// the report carries none for it.
    fn add_clone_outcome(&mut self, key: &str, outcome: Option<&CloneOutcome>) {
        match outcome {
            Some(CloneOutcome::Cloned { .. }) => self.act.add_clone(key, ""),
            Some(CloneOutcome::Held { by }) => self.held.add_clone(key, clone_hold_note(*by)),
            Some(CloneOutcome::CloneFailed { failure }) => self
                .problems
                .failed
                .push(format!("{key} (clone: {})", failure.words(false))),
            Some(CloneOutcome::Failed { message }) => self
                .problems
                .failed
                .push(format!("{key} (clone: {})", first_line(message))),
            None => self
                .problems
                .failed
                .push(format!("{key} (clone: no outcome)")),
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
                    by: BranchSyncHold::Pinned,
                    ..
                }
                | BranchOutcome::Untouched,
            ) => {}
            Some(BranchOutcome::Held { action, by }) => {
                self.held.add(*action, label, hold_note(*by));
            }
            Some(BranchOutcome::PushFailed { failure }) => {
                self.problems
                    .failed
                    .push(format!("{label} (push: {})", failure.words(false)));
            }
            Some(BranchOutcome::Failed { action, message }) => {
                self.problems
                    .failed
                    .push(format!("{label} ({}: {message})", action_verb(*action)));
            }
            Some(BranchOutcome::NeedsHuman { reason }) => {
                self.problems.needs_human.push(format!(
                    "{label} ({})",
                    branch_needs_human_label(*reason, b)
                ));
            }
            None => self
                .problems
                .failed
                .push(format!("{label} ({}: no outcome)", action_verb(action))),
        }
    }

    const fn len(&self) -> usize {
        self.problems.len()
            + self.act.len()
            + self.held.len()
            + self.local_only.len()
            + self.uncommitted.len()
            + self.cleanup.len()
    }
}

/// A branch of `e` as the summary names it: the entry's key alone for the
/// branch it follows, else `key:branch`.
fn branch_label(e: &EntryStatus, b: &BranchStatus) -> String {
    if Some(&b.name) == e.branch.as_ref() {
        e.key.clone()
    } else {
        format!("{}:{}", e.key, b.name)
    }
}

/// Why a branch is cleanup, and the worktree that goes with it when
/// classify found it removable.
fn cleanup_why(
    reason: CleanupReason,
    removable_worktree: Option<&str>,
    b: &BranchStatus,
    view: View<'_>,
) -> String {
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
    why
}

/// A prunable worktree's cleanup item, worded by what classify decided
/// about removing it (`Prune`).
fn prunable_item(
    e: &EntryStatus,
    u: &UnprobedWorktreeStatus,
    workspace: &Path,
    view: View<'_>,
) -> String {
    let key = &e.key;
    let at = view.show(&u.worktree.path);
    match &u.prune {
        // never `git worktree repair <new path>` or `git worktree prune`:
        // both are repo-wide, the one may hijack another checkout and the
        // other drops every gone worktree, so the command is `remove`, this
        // one's alone, and only the scan offers a repair, vetted, for a
        // moved worktree at the root
        Some(Prune::Safe) => format!(
            "{key} (worktree {at} gone — {IF_MOVED}, else git -C {} worktree remove {})",
            view.show_arg(&workspace.join(&e.dir).to_string_lossy()),
            view.show_arg(&u.worktree.path)
        ),
        // the scan found it moved: those strays' lines say what to do,
        // whatever they are, and removing it would orphan them
        Some(Prune::Moved { to }) => {
            let see = if to.len() == 1 {
                "its line"
            } else {
                "their lines"
            };
            format!(
                "{key} (worktree {at} gone — moved to {}; see {see})",
                to.join(", ")
            )
        }
        // classify found removing it would lose something: word it
        loses => {
            let losses = match loses {
                Some(Prune::Loses { losses }) => {
                    losses.iter().map(prune_loss_label).collect::<Vec<_>>()
                }
                _ => vec!["its state".to_owned()],
            };
            format!(
                "{key} (worktree {at} gone — {IF_MOVED}; removing discards {})",
                losses.join(" and ")
            )
        }
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
        (BranchNeedsHuman::ArchivedAhead, Relation::Gone) => {
            "archived, upstream gone from origin".into()
        }
        (BranchNeedsHuman::ArchivedAhead, _) => "archived, no upstream on origin".into(),
        (BranchNeedsHuman::ShallowLocalWork, _) => {
            format!("shallow, tips differ, +{} local", b.unique_commits)
        }
        (BranchNeedsHuman::UpstreamNotABranch, _) => format!(
            "ahead, upstream {} not a branch",
            b.upstream.as_deref().unwrap_or("unknown")
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
