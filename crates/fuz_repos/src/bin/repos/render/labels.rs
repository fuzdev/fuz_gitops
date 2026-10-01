//! Labels and notes shared by the summary, entry, and unregistered views.

use super::*;

/// A remote URL shortened for the summary: `account/name` when it's on the
/// registry URL's host, else the URL as configured.
pub(super) fn compact_remote(origin: &str, registry_url: &str) -> String {
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

pub(super) fn relation_label(r: Relation) -> String {
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
pub(super) fn verdict_label(v: &Verdict) -> Option<String> {
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
pub(super) fn clone_label(verdict: &CloneVerdict) -> String {
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
        parts.push(format!("held{}", clone_held_note(*by)));
    }
    parts.join(" · ")
}

/// An action's verb, as the summary's runs name it.
pub(super) const fn action_verb(action: SyncAction) -> &'static str {
    match action {
        SyncAction::Push { .. } => "push",
        SyncAction::FastForward { .. } => "ff",
        SyncAction::Move => "move",
    }
}

/// What a held action's label carries after it; an entry-level hold has its
/// reason printed on the entry instead.
pub(super) fn held_note(by: BranchHold) -> &'static str {
    hold_note(by.into())
}

/// `held_note` for a held refresh: the one entry-level reason that holds a
/// refresh is origin drift (`refresh_verdict`), named here since the
/// entry's origin-drift line may not print (a probe that failed after
/// reading the config). An origin not over HTTPS has a note of its own,
/// worded from the entry's `origin_not_https` reason: a fetch that is over
/// HTTPS after all reaches another repo.
pub(super) fn refresh_held_note(by: RefreshHold, reasons: &[NeedsHuman]) -> &'static str {
    let elsewhere = || {
        reasons.iter().any(|r| {
            matches!(r, NeedsHuman::OriginNotHttps { fetch_url, .. } if fetches_elsewhere(fetch_url))
        })
    };
    match by {
        RefreshHold::Pinned => " (pinned)",
        RefreshHold::Entry => " (origin drift)",
        RefreshHold::OriginNotHttps if elsewhere() => " (origin elsewhere)",
        RefreshHold::OriginNotHttps => " (origin not HTTPS)",
    }
}

/// Whether an `origin_not_https` reason's fetch URL is HTTPS after all —
/// an `insteadOf` rewrite naming another repo — so its wording names the
/// repo it would reach, not the transport.
fn fetches_elsewhere(fetch_url: &str) -> bool {
    fetch_url.starts_with("https://")
}

/// `held_note` for a hold sync found, the verdict's or its own.
pub(super) const fn hold_note(by: BranchSyncHold) -> &'static str {
    match by {
        BranchSyncHold::Pinned => " (pinned)",
        BranchSyncHold::Entry => "",
        BranchSyncHold::PushUrl => " (push URL)",
        BranchSyncHold::FetchFailed => " (fetch failed)",
        BranchSyncHold::DirtyCheckout => " (dirty)",
        BranchSyncHold::UnprobedWorktree => " (unprobed worktree)",
        BranchSyncHold::SeveralCheckouts => " (several checkouts)",
        BranchSyncHold::Busy => " (busy)",
        BranchSyncHold::BusyUnknown => " (busy unknown)",
        BranchSyncHold::Changed => " (changed since read, rerun)",
    }
}

/// `held_note` for a held clone.
pub(super) fn clone_held_note(by: CloneHold) -> &'static str {
    clone_hold_note(by.into())
}

/// `clone_held_note` for a hold sync found, the verdict's or its own.
pub(super) const fn clone_hold_note(by: CloneSyncHold) -> &'static str {
    match by {
        CloneSyncHold::Entry => "",
        CloneSyncHold::Busy => " (busy)",
        CloneSyncHold::UnprobedWorktree => " (unprobed worktree)",
        CloneSyncHold::Changed => " (changed since read, rerun)",
    }
}

/// Why busy detection is unavailable, as the `failed` line words it.
pub(super) fn unavailable_label(reason: &Unavailable, view: View<'_>) -> String {
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
pub(super) fn session_label(s: &Session, view: View<'_>) -> String {
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
pub(super) fn sessions_label(sessions: &[Session], view: View<'_>) -> String {
    sessions
        .iter()
        .map(|s| session_label(s, view))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What removing a gone worktree would discard, as the cleanup line words
/// it.
pub(super) fn prune_loss_label(loss: &PruneLoss) -> String {
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
pub(super) fn uncommitted_summary(
    key: &str,
    checkouts: &[Checkout],
    view: View<'_>,
) -> Option<String> {
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
pub(super) fn group_digits(n: u64) -> String {
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

pub(super) fn uncommitted_detail(u: &Uncommitted) -> String {
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

/// Why an entry's visibility check couldn't reach its host; `None` when it
/// did, or didn't run.
pub(super) const fn visibility_cause(e: &EntryStatus) -> Option<UnreachableCause> {
    match &e.visibility_check {
        Some(VisibilityCheck::Unknown { failure }) => unreachable_cause(failure),
        _ => None,
    }
}

/// An unreachable host's cause; `None` for any other failure.
pub(super) const fn unreachable_cause(f: &RemoteFailure) -> Option<UnreachableCause> {
    match f {
        RemoteFailure::Unreachable { cause, .. } => Some(*cause),
        _ => None,
    }
}

pub(super) fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim()
}

/// A compact age: `45s`, `12m`, `3h`, `5d`, `4mo`, `2y`.
pub(super) fn format_age(secs: u64) -> String {
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

/// Items as one run, the first carrying `prefix` (`push `, `owned: `); no
/// run when there are none.
pub(super) fn prefixed(prefix: &str, mut items: Vec<String>) -> Option<Vec<String>> {
    let first = items.first_mut()?;
    first.insert_str(0, prefix);
    Some(items)
}

/// An unprobed checkout's HEAD, after its path.
pub(super) fn unprobed_head_label(head: Option<&Head>) -> String {
    match head {
        Some(Head::Branch { name }) => format!(" on {name}"),
        Some(Head::Detached { commit }) => {
            format!(" detached at {}", commit.get(..12).unwrap_or(commit))
        }
        None => " HEAD unreadable".to_owned(),
    }
}

/// A reason's label; an operation outside the primary checkout names the
/// worktree it's in, and an unresolvable checkout says which kind it is.
pub(super) fn needs_human_label(reason: &NeedsHuman, e: &EntryStatus, view: View<'_>) -> String {
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
            unprobed_head_label(head.as_ref()),
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
pub(super) fn fetch_fix(fix: Option<&OriginFix>, expected: &str, dir: &str) -> String {
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

/// A worktree git dir by its id, the last component of
/// `<common>/worktrees/<id>`.
pub(super) fn git_dir_id(git_dir: &str) -> &str {
    Path::new(git_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(git_dir)
}
