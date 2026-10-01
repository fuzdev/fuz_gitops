//! The per-entry views: `--verbose`'s block and `--brief`'s line.

use super::*;

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
        let _ = writeln!(out, "  {:<10}fetch: {}", "error", failure.words(true));
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
                failure.words(true)
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
