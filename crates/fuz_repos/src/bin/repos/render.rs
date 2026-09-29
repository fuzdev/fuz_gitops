//! Text rendering of a `StatusReport`: the grouped summary and `--verbose`'s
//! per-entry blocks.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;

use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::registry::{CheckoutMode, EntryKind, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    EntryStatus, RepairBlock, StatusReport, UnregisteredClone, UnregisteredKind,
};
use fuz_repos::state::{
    BranchNeedsHuman, BranchStatus, CleanupReason, Head, HeldBy, Presence, Prune, PruneLoss,
    Relation, SyncAction, Uncommitted, UnprobedHead, UnprobedWhy, Verdict,
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
    let mut g = Groups::default();
    let mut quiet = Counts::default();
    let workspace = Path::new(&report.workspace);
    for e in &report.entries {
        if g.add(e, workspace, view, verbose) {
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
    let mut line = |label: &str, tone: Tone, items: Items| {
        out.push_str(&render_group(label, tone, &items, view));
    };
    line("visibility", Tone::Red, Items::Singles(g.visibility));
    line("failed", Tone::Red, Items::Singles(g.failed));
    let mut hints = Vec::new();
    if report.entries.iter().any(EntryStatus::probe_failed_partial) {
        hints.push(format!("{} (each under --verbose)", partial_hint("<dir>")));
    }
    let fetch_failed = |pick: fn(&RemoteFailure) -> bool| {
        report
            .entries
            .iter()
            .any(|e| e.fetch_error.as_ref().is_some_and(pick))
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
    let mut sync = g.act.verbs();
    sync.extend(prefixed("clone ", g.clone));
    line("sync would", Tone::Green, Items::Runs(sync, " · "));
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

    /// A run per verb — `push a +1, b +2`, `ff …`, `move …` — omitting empty
    /// verbs.
    fn verbs(&self) -> Vec<Vec<String>> {
        [
            ("push ", &self.push),
            ("ff ", &self.ff),
            ("move ", &self.moves),
        ]
        .into_iter()
        .filter_map(|(verb, items)| prefixed(verb, items.clone()))
        .collect()
    }

    const fn len(&self) -> usize {
        self.push.len() + self.ff.len() + self.moves.len()
    }
}

#[derive(Debug, Default)]
struct Groups {
    visibility: Vec<String>,
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
    fn add(&mut self, e: &EntryStatus, workspace: &Path, view: View<'_>, verbose: bool) -> bool {
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
        self.visibility.len()
            + self.failed.len()
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
            visibility_check: None,
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
        UnprobedWorktreeStatus { worktree, prune }
    }

    fn report(entries: Vec<EntryStatus>) -> StatusReport {
        StatusReport::new(
            "/home/me/dev".into(),
            "/home/me/dev/repos.toml".into(),
            false,
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
        let mut kit = entry("kit", CheckoutMode::Head, "main");
        kit.writable = false;
        kit.url = "https://github.com/sveltejs/kit".into();
        kit.needs_human = vec![NeedsHuman::OriginMismatch {
            origin: OriginRemote::Url {
                url: "https://codeberg.org/someone/kit".into(),
            },
            expected: "https://github.com/sveltejs/kit".into(),
            fix: OriginFix::SetUrl,
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
        let mut spec = entry("spec", CheckoutMode::Head, "x");
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
uncommitted   app (worktree ~/dev/app-feat, 3)  app (worktree ~/wt/app-feat, 1)
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
        ];
        let mut r = report(vec![entry("app", main(), "main")]);
        r.unregistered = Some(strays.clone());
        assert_eq!(
            render_summary(&r, VIEW, false),
            "\
unregistered  owned: app-feat (moved worktree of app — another moved worktree claims this dir; repair that one first once its repair is offered, then rerun),
              s-moved (moved worktree of app — a repair would also rewrite ~/dev/q; fix that first),
              t-moved (moved worktree of app — a relative gitdir in this repo, which git versions resolve differently; fix by hand)
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
        for (key, commits) in [("grimoire", 13), ("setup", 2), ("fuz_util", 1)] {
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
            let mut e = entry(key, main(), "main");
            e.presence = Presence::Missing;
            e.checkouts.clear();
            entries.push(e);
        }
        let r = report(entries);
        assert!(render_summary(&r, VIEW, false).starts_with(
            "sync would    push grimoire +13, setup +2, fuz_util +1 · ff zzz −3 · clone blake3, \
                 corpora\n"
        ));
        assert!(
            render_summary(&r, View { width: 50, ..VIEW }, false).starts_with(
                "\
sync would    push grimoire +13, setup +2,
              fuz_util +1
              ff zzz −3
              clone blake3, corpora
"
            )
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
