//! The unregistered clones: the summary's runs and the per-clone block.

use super::*;

/// The summary's `unregistered` runs — `owned: …`, `third-party: …`,
/// `no origin: …` — each stray by dir name, a worktree marked with what it is
/// and, when moved, its fix. Nothing when the scan didn't run or found none.
pub(super) fn unregistered_groups(report: &StatusReport, view: View<'_>) -> Vec<Vec<String>> {
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
    let path = Path::new(&report.workspace).join(&u.dir);
    let path = path.to_string_lossy();
    let owner = match (u.owned, &u.origin) {
        (true, _) => "owned",
        (false, Some(_)) => "third-party",
        (false, None) => "no origin",
    };
    let (kind, detail) = unregistered_kind_detail(u, &path, report, view);
    let mut out = format!("{}  unregistered · {owner} · {kind}\n", u.dir);
    let _ = writeln!(out, "  {:<10}{}", "dir", view.show(&path));
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

/// A block line after the origin: its label, and what it says.
type DetailLine = (&'static str, String);

/// What an unregistered dir is, for its block's header, and the lines that
/// follow its origin; `path` is the dir's full path.
fn unregistered_kind_detail(
    u: &UnregisteredClone,
    path: &str,
    report: &StatusReport,
    view: View<'_>,
) -> (String, Vec<DetailLine>) {
    match &u.kind {
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
        } => (
            format!("moved worktree of {entry}"),
            repair_detail(entry, exit_noise.as_deref(), path, report, view),
        ),
        UnregisteredKind::MovedWorktree {
            entry,
            blocked_by: Some(block),
            ..
        } => (
            format!("moved worktree of {entry}"),
            repair_block_detail(entry, block, path, view),
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
    }
}

/// A repairable moved worktree's fix, run from its entry's dir, and the
/// note when git will exit 1 over another worktree (`exit_noise`).
fn repair_detail(
    entry: &str,
    exit_noise: Option<&str>,
    path: &str,
    report: &StatusReport,
    view: View<'_>,
) -> Vec<DetailLine> {
    // the entry's dir as a word in the command
    let dir = report
        .entries
        .iter()
        .find(|e| e.key == entry)
        .map_or(entry, |e| e.dir.as_str());
    let entry_dir = view.show_arg(&Path::new(&report.workspace).join(dir).to_string_lossy());
    let mut detail = vec![(
        "fix",
        format!("git -C {entry_dir} worktree repair {}", view.show_arg(path)),
    )];
    if let Some(noise) = exit_noise {
        detail.push((
            "note",
            format!(
                "git will complain about {} and exit 1, leaving it be; this one is repaired all \
                 the same",
                view.show(noise)
            ),
        ));
    }
    detail
}

/// Why a moved worktree of `entry` gets no repair, and the fix when
/// there's a mechanical one.
fn repair_block_detail(
    entry: &str,
    block: &RepairBlock,
    path: &str,
    view: View<'_>,
) -> Vec<DetailLine> {
    let note = match block {
        RepairBlock::Rewrites {
            path: other,
            git_dir,
        } => format!(
            "git worktree repair would also rewrite {}/.git — {entry}'s worktree git dir {} \
             names it, and its .git is missing or names another — fix that first",
            view.show(other),
            git_dir_id(git_dir)
        ),
        RepairBlock::Swapped { git_dir, with } => format!(
            "swapped by hand with {with}: {entry}'s worktree git dir {} names this dir while \
             {with}'s .git names it — move the two dirs back; a repair of either would hijack \
             the other",
            git_dir_id(git_dir)
        ),
        RepairBlock::ClaimedDir { git_dir } => format!(
            "{entry}'s worktree git dir {} names this dir, so a repair would point this .git \
             there — repair the moved worktree whose .git names it first, once its repair is \
             offered, then rerun repos status",
            git_dir_id(git_dir)
        ),
        RepairBlock::RelativeGitdir { git_dir } => format!(
            "{entry}'s worktree git dir {} names its worktree by a relative path, which git \
             2.48+ resolves against the git dir and older gits against the cwd — what a repair \
             would touch is uncertain, so none is offered; make that gitdir absolute by hand, \
             then rerun repos status",
            git_dir_id(git_dir)
        ),
        RepairBlock::UnreadableGitdir { git_dir } => format!(
            "{entry}'s worktree git dir {} has a gitdir that can't be read by this tool \
             (unreadable, or past its size limit, which git may read fine) — what a repair \
             would touch is unknown, so none is offered; trim or fix it by hand, then rerun \
             repos status",
            git_dir_id(git_dir)
        ),
        RepairBlock::NulInGitdir { git_dir } => {
            return vec![
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
                        view.show_arg(&Path::new(path).join(".git").to_string_lossy()),
                        view.show_arg(&Path::new(git_dir).join("gitdir").to_string_lossy())
                    ),
                ),
            ];
        }
        RepairBlock::NonUtf8Path => "its path isn't UTF-8, so no repair command here can name \
                                     it exactly — rename it to a UTF-8 name, then rerun repos \
                                     status"
            .to_owned(),
    };
    vec![("note", note)]
}
