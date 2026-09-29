//! Text rendering of a `StatusReport`: the grouped summary and `--verbose`'s
//! per-entry blocks.

use std::fmt::Write as _;
use std::path::Path;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::registry::{CheckoutMode, EntryKind, Visibility};
use fuz_repos::report::{
    EntryStatus, RepairBlock, StatusReport, UnregisteredClone, UnregisteredKind,
};
use fuz_repos::state::{
    BranchNeedsHuman, BranchStatus, CleanupReason, Head, HeldBy, Presence, Prune, PruneLoss,
    Relation, SyncAction, Uncommitted, UnprobedHead, UnprobedWhy, Verdict,
};

/// The label column's width.
const LABEL_WIDTH: usize = 13;

/// What rendering needs from the environment: the home dir, shown as `~`,
/// and the current time, which the report's timestamps become ages against.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub home: Option<&'a str>,
    /// Unix seconds.
    pub now: u64,
}

impl View<'_> {
    /// A timestamp's compact age.
    fn age(&self, at: u64) -> String {
        format_age(self.now.saturating_sub(at))
    }

    pub fn show(&self, path: &str) -> String {
        match self.home {
            Some(home) if !home.is_empty() => match path.strip_prefix(home) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => path.to_owned(),
            },
            _ => path.to_owned(),
        }
    }
}

/// The grouped summary: what to act on, the quiet entries as counts, and the
/// footer. A workspace with nothing to act on prints just the last line.
pub fn render_summary(report: &StatusReport, view: View<'_>, verbose: bool) -> String {
    let mut g = Groups::default();
    let mut quiet = Counts::default();
    for e in &report.entries {
        if g.add(e, view, verbose) {
            continue;
        }
        match (&e.checkout_mode, e.checkouts.first().map(|c| &c.head)) {
            (CheckoutMode::Pinned, _) => quiet.pinned += 1,
            (CheckoutMode::Follow { branch }, Some(Head::Branch { name })) if name != branch => {
                quiet.on_branches += 1;
            }
            _ => quiet.clean += 1,
        }
    }

    let mut out = String::new();
    let mut line = |label: &str, items: &[String], sep: &str| {
        if !items.is_empty() {
            let _ = writeln!(out, "{label:<LABEL_WIDTH$} {}", items.join(sep));
        }
    };
    line("failed", &g.failed, "  ");
    line("needs human", &g.needs_human, "  ");
    line("origin drift", &g.origin_drift, "  ");
    if !g.origin_drift.is_empty() {
        let hint = "hint: git -C <dir> remote set-url origin <url> (each under --verbose)";
        line("", &[hint.to_owned()], "");
    }
    let mut sync = g.act.verbs();
    if !g.clone.is_empty() {
        sync.push(format!("clone {}", g.clone.join(", ")));
    }
    line("sync would", &sync, " · ");
    line("held", &g.held.verbs(), " · ");
    line("local-only", &g.local_only, "  ");
    line("uncommitted", &g.uncommitted, "  ");
    line("cleanup", &g.cleanup, "  ");
    line("unregistered", &unregistered_groups(report, view), "  ");
    line("stashes", &g.stashes, "  ");

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

/// Sync actions by verb, each item a labeled branch.
#[derive(Debug, Default)]
struct Actions {
    push: Vec<String>,
    ff: Vec<String>,
    moves: Vec<String>,
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

    /// `push a +1, b +2`, `ff …`, `move …`, omitting empty verbs.
    fn verbs(&self) -> Vec<String> {
        [
            ("push", &self.push),
            ("ff", &self.ff),
            ("move", &self.moves),
        ]
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(verb, items)| format!("{verb} {}", items.join(", ")))
        .collect()
    }

    const fn len(&self) -> usize {
        self.push.len() + self.ff.len() + self.moves.len()
    }
}

#[derive(Debug, Default)]
struct Groups {
    failed: Vec<String>,
    needs_human: Vec<String>,
    origin_drift: Vec<String>,
    act: Actions,
    held: Actions,
    clone: Vec<String>,
    local_only: Vec<String>,
    uncommitted: Vec<String>,
    cleanup: Vec<String>,
    stashes: Vec<String>,
}

impl Groups {
    /// Adds an entry's lines; returns whether it had anything to say.
    fn add(&mut self, e: &EntryStatus, view: View<'_>, verbose: bool) -> bool {
        let before = self.len();
        let key = &e.key;
        let follow = match &e.checkout_mode {
            CheckoutMode::Follow { branch } => Some(branch.as_str()),
            CheckoutMode::Pinned | CheckoutMode::Head => None,
        };
        let label = |b: &BranchStatus| {
            if Some(b.name.as_str()) == follow {
                key.clone()
            } else {
                format!("{key}:{}", b.name)
            }
        };

        if let Some(error) = &e.probe_error {
            self.failed
                .push(format!("{key} (probe: {})", first_line(error)));
        }
        if let Some(error) = &e.fetch_error {
            self.failed
                .push(format!("{key} (fetch: {})", first_line(error)));
        }
        for reason in &e.needs_human {
            match reason {
                NeedsHuman::OriginMismatch { origin, .. } => {
                    let was = origin
                        .as_deref()
                        .map_or_else(|| "no origin".to_owned(), |o| compact_remote(o, &e.url));
                    self.origin_drift.push(format!("{key} ({was})"));
                }
                reason => self
                    .needs_human
                    .push(format!("{key} ({})", needs_human_label(reason, e, view))),
            }
        }
        if e.presence == Presence::Missing {
            self.clone.push(key.clone());
        }
        for b in &e.branches {
            match &b.verdict {
                Verdict::Quiet => {}
                Verdict::Act { action } => self.act.add(*action, &label(b), ""),
                Verdict::Held { action, by } => {
                    self.held.add(*action, &label(b), held_note(*by));
                }
                Verdict::NeedsHuman { reason } => {
                    let why = match (*reason, b.relation) {
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
                    };
                    self.needs_human.push(format!("{} ({why})", label(b)));
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
        for u in &e.unprobed_worktrees {
            let at = view.show(&u.worktree.path);
            match (&u.worktree.why, &u.prune) {
                (UnprobedWhy::Failed { error }, _) => self
                    .failed
                    .push(format!("{key} (worktree {at}: {})", first_line(error))),
                (UnprobedWhy::Prunable, Some(Prune::Safe)) => self.cleanup.push(format!(
                    "{key} (worktree {at} gone — git worktree prune, \
                     or git worktree repair <new path> if it moved)"
                )),
                // classify found pruning would lose something: word it
                (UnprobedWhy::Prunable, loses) => {
                    let losses = match loses {
                        Some(Prune::Loses { losses }) => {
                            losses.iter().map(prune_loss_label).collect::<Vec<_>>()
                        }
                        _ => vec!["its state".to_owned()],
                    };
                    self.cleanup.push(format!(
                        "{key} (worktree {at} gone — git worktree repair <new path> if it \
                         moved; pruning discards {})",
                        losses.join(" and ")
                    ));
                }
                // intentional, as on unmounted media: `--verbose` shows it,
                // and it still holds its branch
                (UnprobedWhy::Missing, _) => {}
            }
        }
        for c in &e.checkouts {
            if !c.uncommitted.is_clean() {
                let detail = if verbose {
                    uncommitted_detail(&c.uncommitted)
                } else {
                    c.uncommitted.total().to_string()
                };
                // another worktree by its shown path, beside the primary's key
                let label = if c.primary {
                    format!("{key} ({detail})")
                } else {
                    format!("{key} (worktree {}, {detail})", view.show(&c.path))
                };
                self.uncommitted.push(label);
            }
        }
        let said = self.len() > before;
        if verbose && e.stashes > 0 {
            self.stashes.push(format!("{key} ({})", e.stashes));
        }
        said
    }

    const fn len(&self) -> usize {
        self.failed.len()
            + self.needs_human.len()
            + self.origin_drift.len()
            + self.act.len()
            + self.held.len()
            + self.clone.len()
            + self.local_only.len()
            + self.uncommitted.len()
            + self.cleanup.len()
    }
}

/// A reason's label; an operation outside the primary checkout names the
/// worktree it's in.
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
        NeedsHuman::OriginMismatch {
            origin: Some(origin),
            ..
        } => format!("origin is {origin}"),
        NeedsHuman::OriginMismatch { origin: None, .. } => "no origin".into(),
        NeedsHuman::WorktreeUnreadable { path } => {
            format!("worktree git dir unreadable: {}", view.show(path))
        }
        NeedsHuman::DefaultBranchMissing { branch } => format!("no local {branch}"),
        NeedsHuman::DefaultBranchNoUpstream { branch } => {
            format!("{branch} has no origin upstream")
        }
        NeedsHuman::UnexpectedDetached { .. } => "detached".into(),
        NeedsHuman::PinnedOnBranch { branch } => format!("pinned, on {branch}"),
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
                && e.checkout_mode != CheckoutMode::Pinned
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
/// by eye.
pub fn render_entry(e: &EntryStatus, view: View<'_>) -> String {
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
    tags.push(match &e.checkout_mode {
        CheckoutMode::Follow { branch } => format!("follow {branch}"),
        CheckoutMode::Pinned => "pinned".into(),
        CheckoutMode::Head => "leave HEAD".into(),
    });
    let _ = writeln!(out, "{}  {}", e.key, tags.join(" · "));
    let _ = writeln!(out, "  {:<10}{}", "url", e.url);

    match e.presence {
        Presence::Missing => {
            let _ = writeln!(out, "  {:<10}missing: {}", "dir", e.dir);
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
        let _ = writeln!(
            out,
            "  {:<10}{}{mark} {head} · {dirt}{op}",
            "checkout",
            view.show(&c.path)
        );
    }
    for u in e.unprobed_worktrees.iter().map(|u| &u.worktree) {
        let why = match u.why {
            UnprobedWhy::Prunable => "prunable",
            UnprobedWhy::Missing => "missing",
            UnprobedWhy::Failed { .. } => "probe failed",
        };
        let locked = if u.locked { ", locked" } else { "" };
        let head = match &u.head {
            UnprobedHead::Branch { name } => format!(" on {name}"),
            UnprobedHead::Detached { commit } => {
                format!(" detached at {}", commit.get(..12).unwrap_or(commit))
            }
            UnprobedHead::Unknown => " HEAD unreadable".to_owned(),
        };
        let op = u
            .in_progress
            .map(|op| format!(" · {} in progress", op.label()))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:<10}{} (worktree, {why}{locked}){head}{op}",
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
    let dir = e
        .checkouts
        .first()
        .map_or_else(|| e.dir.clone(), |c| view.show(&c.path));
    for reason in &e.needs_human {
        let detail = match reason {
            NeedsHuman::NotARepo { detail } => format!("not a repo: {detail}"),
            NeedsHuman::OriginMismatch { expected, .. } => format!(
                "{} — git -C {dir} remote set-url origin {expected}",
                needs_human_label(reason, e, view)
            ),
            reason => needs_human_label(reason, e, view),
        };
        let _ = writeln!(out, "  {:<10}{detail}", "needs");
    }
    if let Some(error) = &e.probe_error {
        let _ = writeln!(out, "  {:<10}probe: {error}", "error");
    }
    if let Some(error) = &e.fetch_error {
        let _ = writeln!(out, "  {:<10}fetch: {error}", "error");
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

/// The summary's `unregistered` groups — `owned: …`, `third-party: …`,
/// `no origin: …` — each stray by dir name, a worktree marked with what it is
/// and, when moved, its fix. Nothing when the scan didn't run or found none.
fn unregistered_groups(report: &StatusReport, view: View<'_>) -> Vec<String> {
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
        };
        match (u.owned, &u.origin) {
            (true, _) => owned.push(label),
            (false, Some(_)) => third_party.push(label),
            (false, None) => no_origin.push(label),
        }
    }
    [
        ("owned", owned),
        ("third-party", third_party),
        ("no origin", no_origin),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(group, items)| format!("{group}: {}", items.join(", ")))
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
            "another moved worktree claims this dir; repair it first, then rerun".to_owned()
        }
        RepairBlock::Swapped { with, .. } => format!("swapped with {with}; move the dirs back"),
    }
}

/// `--verbose`'s block for one unregistered dir: what it is, its origin,
/// and the fix when there's a mechanical one.
pub fn render_unregistered(u: &UnregisteredClone, report: &StatusReport, view: View<'_>) -> String {
    let workspace = Path::new(&report.workspace);
    let at = view.show(&workspace.join(&u.dir).to_string_lossy());
    let entry_dir = |key: &str| {
        let dir = report
            .entries
            .iter()
            .find(|e| e.key == key)
            .map_or(key, |e| e.dir.as_str());
        view.show(&workspace.join(dir).to_string_lossy())
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
                format!("git -C {} worktree repair {at}", entry_dir(entry)),
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
                     .git there — repair the moved worktree whose .git names it first, then \
                     rerun repos status",
                    git_dir_id(git_dir)
                ),
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
    let verb = |a| match a {
        SyncAction::Push { .. } => "push",
        SyncAction::FastForward { .. } => "ff",
        SyncAction::Move => "move",
    };
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

/// What a held action's label carries after it; an entry-level hold has its
/// reason printed on the entry instead.
const fn held_note(by: HeldBy) -> &'static str {
    match by {
        HeldBy::Entry => "",
        HeldBy::DirtyCheckout => " (dirty)",
        HeldBy::UnprobedWorktree => " (unprobed worktree)",
    }
}

/// What a prune would discard, as the cleanup line words it.
fn prune_loss_label(loss: &PruneLoss) -> String {
    match loss {
        PruneLoss::Operation { op } => format!("its {} in progress", op.label()),
        PruneLoss::DetachedHead => "its detached HEAD".into(),
        PruneLoss::UnknownHead => "its HEAD".into(),
        PruneLoss::MissingBranch { name } => format!("its HEAD (branch {name} is gone)"),
    }
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

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim()
}

/// A compact age: `45s`, `12m`, `3h`, `5d`, `4mo`, `2y`.
pub fn format_age(secs: u64) -> String {
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
    use fuz_repos::state::{
        Checkout, InProgressOp, Layout, UnprobedWorktree, UnprobedWorktreeStatus,
    };

    use super::*;

    fn entry(key: &str, mode: CheckoutMode, head: &str) -> EntryStatus {
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
            }],
            branches: vec![],
            stashes: 0,
            fetched_at: Some(NOW - 3 * 3600),
            needs_human: vec![],
            probe_error: None,
            unprobed_worktrees: vec![],
            fetch_error: None,
        }
    }

    fn main() -> CheckoutMode {
        CheckoutMode::Follow {
            branch: "main".into(),
        }
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
        }
    }

    /// An unprobed worktree at `path`.
    fn unprobed(path: &str, branch: Option<&str>, why: UnprobedWhy) -> UnprobedWorktree {
        UnprobedWorktree {
            path: path.into(),
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
        }
    }

    /// An unprobed worktree as the report carries it.
    const fn status(worktree: UnprobedWorktree, prune: Option<Prune>) -> UnprobedWorktreeStatus {
        UnprobedWorktreeStatus { worktree, prune }
    }

    fn report(entries: Vec<EntryStatus>) -> StatusReport {
        StatusReport::new(
            "/home/me/dev".into(),
            "/home/me/dev/repos.toml".into(),
            entries,
        )
    }

    const NOW: u64 = 1_800_000_000;

    const VIEW: View<'static> = View {
        home: Some("/home/me"),
        now: NOW,
    };

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
            checkout_mode: CheckoutMode::Pinned,
            writable: false,
            ..entry("oracle", CheckoutMode::Pinned, "x")
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
        let mut blake3 = entry("blake3", main(), "main");
        blake3.presence = Presence::Missing;
        blake3.checkouts.clear();
        let mut old = entry("old", main(), "main");
        old.archived = true;
        old.branches = vec![branch(
            "main",
            Some("origin/main"),
            Relation::Ahead { commits: 1 },
            1,
            needs(BranchNeedsHuman::ArchivedAhead),
        )];
        let mut svelte = entry("svelte", CheckoutMode::Pinned, "x");
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
        let mut wpt = entry(
            "wpt",
            CheckoutMode::Follow {
                branch: "fork".into(),
            },
            "fork",
        );
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
        wpt.fetch_error = Some("fatal: couldn't find remote ref fork\n".into());

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
failed        wpt (fetch: fatal: couldn't find remote ref fork)
needs human   uz:arc (diverged +2 −5)  old (archived, +1)  wpt (rebase in progress)  wpt (outside refspec)
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
            origin: Some("git@github.com:ryanatkn/fuz_blog".into()),
            expected: "git@github.com:fuzdev/fuz_blog".into(),
        }];
        let mut kit = entry("kit", CheckoutMode::Head, "main");
        kit.writable = false;
        kit.url = "https://github.com/sveltejs/kit".into();
        kit.needs_human = vec![NeedsHuman::OriginMismatch {
            origin: Some("https://codeberg.org/someone/kit".into()),
            expected: "https://github.com/sveltejs/kit".into(),
        }];
        let mut test262 = entry("test262", CheckoutMode::Head, "x");
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
        let mut goblins = entry("goblins", CheckoutMode::Head, "x");
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
        let blog_block = render_entry(&r.entries[0], VIEW);
        assert!(
            blog_block.contains(
                "  needs     origin is git@github.com:ryanatkn/fuz_blog — git -C ~/dev/fuz_blog \
                 remote set-url origin git@github.com:fuzdev/fuz_blog\n"
            ),
            "{blog_block}"
        );
        let goblins_block = render_entry(&r.entries[3], VIEW);
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
        let mut spec = entry("spec", CheckoutMode::Head, "x");
        spec.kind = EntryKind::Reference;
        spec.fetched_at = Some(NOW - 90 * 86400);
        assert!(
            render_entry(&gro, VIEW).contains("behind 1 · 2d → held ff (dirty)\n"),
            "{}",
            render_entry(&gro, VIEW)
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
            render_entry(&e, VIEW),
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
            // pruning would lose something: classify said what
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
        ];
        let r = report(vec![app]);
        // the missing worktree says nothing here but holds its branch
        assert_eq!(
            render_summary(&r, VIEW, false),
            "\
failed        app (worktree ~/dev/app-broken: git status failed (128): fatal: not a git repository)
held          ff app:feat −1 (dirty), app:usb −4 (unprobed worktree)
uncommitted   app (worktree ~/dev/app-feat, 3)  app (worktree ~/wt/app-feat, 1)
cleanup       app:old (upstream gone, worktree ~/wt/app-old removable)  app (worktree ~/dev/app-gone gone — git worktree prune, or git worktree repair <new path> if it moved)  app (worktree ~/dev/app-spike gone — git worktree repair <new path> if it moved; pruning discards its detached HEAD)  app (worktree ~/moved-fix gone — git worktree repair <new path> if it moved; pruning discards its rebase in progress and its detached HEAD)  app (worktree ~/dev/app-deleted gone — git worktree repair <new path> if it moved; pruning discards its HEAD (branch feat is gone))  app (worktree ~/dev/app-garbled gone — git worktree repair <new path> if it moved; pruning discards its HEAD)
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
        );
        assert!(
            render_summary(&r, VIEW, true)
                .contains("uncommitted   app (worktree ~/dev/app-feat, 2 unstaged, 1 untracked)")
        );
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
        ];
        assert_eq!(
            render_entry(&e, VIEW),
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
unregistered  owned: app-copy (shares app's git dir with ~/wt/app-feat — don't repair), app-old \
(moved worktree of app — git worktree repair), mine  third-party: lib, lib-feat (worktree)  no \
origin: site-orphan (orphaned worktree of site — its git dir is lost)
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
        ];
        let mut r = report(vec![entry("app", main(), "main")]);
        r.unregistered = Some(strays.clone());
        assert_eq!(
            render_summary(&r, VIEW, false),
            "\
unregistered  owned: app-feat (moved worktree of app — another moved worktree claims this dir; \
repair it first, then rerun), s-moved (moved worktree of app — a repair would also rewrite \
~/dev/q; fix that first)
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
  note      app's worktree git dir app-feat names this dir, so a repair would point this .git there — repair the moved worktree whose .git names it first, then rerun repos status
s-moved  unregistered · owned · moved worktree of app
  dir       ~/dev/s-moved
  origin    git@github.com:me/app
  note      git worktree repair would also rewrite ~/dev/q/.git — app's worktree git dir q names it, and its .git is missing or names another — fix that first
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
unregistered  owned: s-moved (moved worktree of app — git worktree repair), wa (moved worktree of \
app — swapped with wb; move the dirs back)
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
        assert_eq!(View { home: None, now: 0 }.show("/x"), "/x");
    }
}
