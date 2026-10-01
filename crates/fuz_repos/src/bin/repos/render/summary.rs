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
            failed.push(format!("{key} (fetch: {})", failure.words(false)));
        }
        match &e.visibility_check {
            Some(VisibilityCheck::Leak) => {
                visibility.push(format!("{key} (declared private, anonymously readable)"));
            }
            Some(VisibilityCheck::Unknown { failure }) => failed.push(format!(
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
            PushOutcome::PushFailed { failure } => {
                failed.push(format!("{label} (push: {})", failure.words(false)));
            }
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
            Some(CloneOutcome::CloneFailed { failure }) => self
                .failed
                .push(format!("{key} (clone: {})", failure.words(false))),
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
                self.failed
                    .push(format!("{label} (push: {})", failure.words(false)));
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
