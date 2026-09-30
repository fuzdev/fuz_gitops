//! Text rendering of a `StatusReport` — the grouped summary,
//! `--verbose`'s per-entry blocks, and `--brief`'s line on one checkout —
//! of a `SyncReport`, whose summary is the same with what sync did in
//! place of what it would do, and of a `PushReport`, its targets' pushes
//! in the same words.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;

use fuz_repos::classify::{NeedsHuman, OriginByHand, OriginFix, OriginRemote};
use fuz_repos::registry::{EntryKind, Visibility};
use fuz_repos::remote::{RefGoneFix, RemoteFailure, UnreachableCause, VisibilityCheck};
use fuz_repos::report::{
    BranchOutcome, CloneOutcome, EntryStatus, EntrySync, FetchOutcome, PushOutcome, PushReport,
    RepairBlock, Sessions, StatusReport, SyncHold, SyncReport, UnregisteredClone, UnregisteredKind,
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

mod entry;
mod labels;
mod summary;
mod unregistered;

pub use entry::{render_brief, render_entry};
pub use summary::{render_push_summary, render_summary, render_sync_summary};
pub use unregistered::render_unregistered;

use labels::{
    action_verb, clone_label, compact_remote, fetch_fix, first_line, format_age, git_dir_id,
    held_note, hold_note, needs_human_label, prefixed, prune_loss_label, refresh_held_note,
    relation_label, remote_failure_label, session_label, sessions_label, unavailable_label,
    uncommitted_detail, uncommitted_summary, unprobed_head_label, unreachable_cause, verdict_label,
    visibility_cause,
};
use unregistered::unregistered_groups;

#[cfg(test)]
mod tests;
