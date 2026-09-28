//! Text rendering of a `StatusReport`: the grouped summary and `--verbose`'s
//! per-entry blocks.

use std::fmt::Write as _;

use fuz_repos::classify::NeedsHuman;
use fuz_repos::registry::{CheckoutMode, EntryKind, Visibility};
use fuz_repos::report::{EntryStatus, StatusReport};
use fuz_repos::state::{BranchStatus, Head, Presence, Relation, Uncommitted};

/// The label column's width.
const LABEL_WIDTH: usize = 13;

/// Renders paths with the home dir as `~`.
#[derive(Debug, Clone, Copy)]
pub struct Paths<'a> {
    pub home: Option<&'a str>,
}

impl Paths<'_> {
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
pub fn render_summary(report: &StatusReport, paths: Paths<'_>, verbose: bool) -> String {
    let mut g = Groups::default();
    let mut quiet = Counts::default();
    for e in &report.entries {
        if g.add(e, verbose) {
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
    let sync: Vec<String> = [
        ("push", &g.push),
        ("ff", &g.ff),
        ("move", &g.moves),
        ("clone", &g.clone),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(verb, items)| format!("{verb} {}", items.join(", ")))
    .collect();
    line("sync would", &sync, " · ");
    line("local-only", &g.local_only, "  ");
    line("uncommitted", &g.uncommitted, "  ");
    line("cleanup", &g.cleanup, "  ");
    line("stashes", &g.stashes, "  ");

    let counts = format!(
        "clean {} · on branches {} · pinned {}",
        quiet.clean, quiet.on_branches, quiet.pinned
    );
    let _ = writeln!(out, "{counts}      {}", footer(report, paths));
    out
}

#[derive(Debug, Default)]
struct Counts {
    clean: u32,
    on_branches: u32,
    pinned: u32,
}

#[derive(Debug, Default)]
struct Groups {
    failed: Vec<String>,
    needs_human: Vec<String>,
    origin_drift: Vec<String>,
    push: Vec<String>,
    ff: Vec<String>,
    moves: Vec<String>,
    clone: Vec<String>,
    local_only: Vec<String>,
    uncommitted: Vec<String>,
    cleanup: Vec<String>,
    stashes: Vec<String>,
}

impl Groups {
    /// Adds an entry's lines; returns whether it had anything to say.
    fn add(&mut self, e: &EntryStatus, verbose: bool) -> bool {
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
                    .push(format!("{key} ({})", needs_human_label(reason))),
            }
        }
        if e.presence == Presence::Missing {
            self.clone.push(key.clone());
        }
        for b in &e.branches {
            match b.relation {
                Relation::Diverged { ahead, behind } => {
                    self.needs_human
                        .push(format!("{} (diverged +{ahead} −{behind})", label(b)));
                }
                Relation::Unmapped => self
                    .needs_human
                    .push(format!("{} (outside refspec)", label(b))),
                // nothing local at stake: the branch can move to the fetched tip
                Relation::Shallow if b.unique_commits == 0 => self.moves.push(label(b)),
                Relation::Shallow => {
                    self.needs_human.push(format!(
                        "{} (shallow, tips differ, +{} local)",
                        label(b),
                        b.unique_commits
                    ));
                }
                Relation::Ahead { commits } if e.archived => {
                    self.needs_human
                        .push(format!("{} (archived, +{commits})", label(b)));
                }
                Relation::Ahead { commits } if e.writable => {
                    self.push.push(format!("{} +{commits}", label(b)));
                }
                Relation::Behind { commits } if e.writable => {
                    self.ff.push(format!("{} −{commits}", label(b)));
                }
                Relation::Gone if e.writable => {
                    let unique = if b.unique_commits > 0 {
                        format!(", +{}", b.unique_commits)
                    } else {
                        String::new()
                    };
                    self.cleanup
                        .push(format!("{} (upstream gone{unique})", label(b)));
                }
                Relation::Untracked if b.unique_commits > 0 => {
                    let read_only = if e.writable { "" } else { ", read-only" };
                    self.local_only.push(format!(
                        "{} (+{}, {}{read_only})",
                        label(b),
                        b.unique_commits,
                        format_age(b.newest_commit_age_secs)
                    ));
                }
                // nothing unique and no upstream: merged, unless it's the
                // default branch (a needs-human reason) or checked out
                Relation::Untracked
                    if e.writable
                        && b.upstream.is_none()
                        && b.worktree.is_none()
                        && Some(b.name.as_str()) != follow =>
                {
                    self.cleanup.push(format!("{} (merged)", label(b)));
                }
                Relation::InSync
                | Relation::Ahead { .. }
                | Relation::Behind { .. }
                | Relation::Gone
                | Relation::Untracked => {}
            }
        }
        for c in &e.checkouts {
            if !c.uncommitted.is_clean() {
                let detail = if verbose {
                    uncommitted_detail(&c.uncommitted)
                } else {
                    c.uncommitted.total().to_string()
                };
                self.uncommitted.push(format!("{key} ({detail})"));
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
            + self.push.len()
            + self.ff.len()
            + self.moves.len()
            + self.clone.len()
            + self.local_only.len()
            + self.uncommitted.len()
            + self.cleanup.len()
    }
}

fn needs_human_label(reason: &NeedsHuman) -> String {
    match reason {
        NeedsHuman::NotARepo { .. } => "not a repo".into(),
        NeedsHuman::OperationInProgress { op, .. } => format!("{} in progress", op.label()),
        NeedsHuman::OriginMismatch {
            origin: Some(origin),
            ..
        } => format!("origin is {origin}"),
        NeedsHuman::OriginMismatch { origin: None, .. } => "no origin".into(),
        NeedsHuman::DefaultBranchMissing { branch } => format!("no local {branch}"),
        NeedsHuman::DefaultBranchNoUpstream { branch } => {
            format!("{branch} has no origin upstream")
        }
        NeedsHuman::UnexpectedDetached { .. } => "detached".into(),
        NeedsHuman::PinnedOnBranch { branch } => format!("pinned, on {branch}"),
    }
}

/// The registry, and how fresh the remote view is: the oldest fetch among
/// the entries sync fetches (owned, active, not pinned), with never-fetched
/// ones counted apart.
fn footer(report: &StatusReport, paths: Paths<'_>) -> String {
    let fetched: Vec<Option<u64>> = report
        .entries
        .iter()
        .filter(|e| {
            e.writable
                && !e.archived
                && e.checkout_mode != CheckoutMode::Pinned
                && e.presence == Presence::Present
                && e.probe_error.is_none()
        })
        .map(|e| e.fetched_age_secs)
        .collect();
    let never = fetched.iter().filter(|a| a.is_none()).count();
    let oldest = fetched.iter().flatten().max();
    let freshness = match (oldest, never) {
        (None, 0) => None,
        (None, _) => Some("never fetched".to_owned()),
        (Some(age), 0) => Some(format!("fetched {} ago", format_age(*age))),
        (Some(age), n) => Some(format!("fetched {} ago, {n} never", format_age(*age))),
    };
    let registry = paths.show(&report.registry);
    freshness.map_or_else(|| registry.clone(), |f| format!("{registry} · {f}"))
}

/// `--verbose`'s block for one entry, to check a classification against git
/// by eye.
pub fn render_entry(e: &EntryStatus, paths: Paths<'_>) -> String {
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
            let mut facts = vec![e.fetched_age_secs.map_or_else(
                || "never fetched".to_owned(),
                |age| format!("fetched {} ago", format_age(age)),
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
        let _ = writeln!(
            out,
            "  {:<10}{} {head} · {dirt}{op}",
            "checkout",
            paths.show(&c.path)
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
        let _ = write!(detail, " · {}", format_age(b.newest_commit_age_secs));
        if b.worktree.is_some() {
            detail.push_str(" · checked out");
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
        .map_or_else(|| e.dir.clone(), |c| paths.show(&c.path));
    for reason in &e.needs_human {
        let detail = match reason {
            NeedsHuman::NotARepo { detail } => format!("not a repo: {detail}"),
            NeedsHuman::OriginMismatch { expected, .. } => format!(
                "{} — git -C {dir} remote set-url origin {expected}",
                needs_human_label(reason)
            ),
            reason => needs_human_label(reason),
        };
        let _ = writeln!(out, "  {:<10}{detail}", "needs");
    }
    if let Some(error) = &e.probe_error {
        let _ = writeln!(out, "  {:<10}probe: {error}", "error");
    }
    if let Some(error) = &e.fetch_error {
        let _ = writeln!(out, "  {:<10}fetch: {error}", "error");
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
    use fuz_repos::state::{Checkout, Layout};

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
            }],
            branches: vec![],
            stashes: 0,
            fetched_age_secs: Some(3 * 3600),
            needs_human: vec![],
            probe_error: None,
            fetch_error: None,
        }
    }

    fn main() -> CheckoutMode {
        CheckoutMode::Follow {
            branch: "main".into(),
        }
    }

    fn branch(name: &str, upstream: Option<&str>, relation: Relation, unique: u32) -> BranchStatus {
        BranchStatus {
            name: name.into(),
            upstream: upstream.map(str::to_owned),
            worktree: None,
            unique_commits: unique,
            newest_commit_age_secs: 2 * 86400,
            relation,
        }
    }

    fn report(entries: Vec<EntryStatus>) -> StatusReport {
        StatusReport::new(
            "/home/me/dev".into(),
            "/home/me/dev/repos.toml".into(),
            entries,
        )
    }

    const PATHS: Paths<'static> = Paths {
        home: Some("/home/me"),
    };

    #[test]
    fn a_clean_workspace_is_one_line() {
        let mut feature = entry("site", main(), "feature");
        feature.branches = vec![branch(
            "feature",
            Some("origin/feature"),
            Relation::InSync,
            0,
        )];
        let pinned = EntryStatus {
            checkout_mode: CheckoutMode::Pinned,
            writable: false,
            ..entry("oracle", CheckoutMode::Pinned, "x")
        };
        let out = render_summary(
            &report(vec![entry("app", main(), "main"), feature, pinned]),
            PATHS,
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
            ),
            branch(
                "arc",
                Some("origin/arc"),
                Relation::Diverged {
                    ahead: 2,
                    behind: 5,
                },
                2,
            ),
            branch("old", Some("origin/old"), Relation::Gone, 1),
            branch("done", None, Relation::Untracked, 0),
            branch("wip", None, Relation::Untracked, 1),
            branch("theirs", Some("upstream/main"), Relation::Untracked, 0),
        ];
        uz.checkouts[0].uncommitted.unstaged = 1;
        uz.stashes = 2;
        let mut zzz = entry("zzz", main(), "main");
        zzz.branches = vec![branch(
            "main",
            Some("origin/main"),
            Relation::Behind { commits: 3 },
            0,
        )];
        zzz.fetched_age_secs = None;
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
        )];
        let mut svelte = entry("svelte", CheckoutMode::Pinned, "x");
        svelte.writable = false;
        svelte.checkouts[0].head = Head::Detached {
            commit: "abc".into(),
        };
        svelte.branches = vec![branch("audit", None, Relation::Untracked, 4)];
        let mut wpt = entry(
            "wpt",
            CheckoutMode::Follow {
                branch: "fork".into(),
            },
            "fork",
        );
        wpt.branches = vec![branch("fork", Some("origin/fork"), Relation::Unmapped, 3)];
        wpt.needs_human = vec![NeedsHuman::OperationInProgress {
            checkout: "/home/me/dev/wpt".into(),
            op: fuz_repos::state::InProgressOp::Rebase,
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
        let out = render_summary(&r, PATHS, false);
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

        let verbose = render_summary(&r, PATHS, true);
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
            branch("main", Some("origin/main"), Relation::Shallow, 0),
            branch("work", Some("origin/work"), Relation::Shallow, 2),
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
            render_summary(&r, PATHS, false),
            "\
needs human   test262:work (shallow, tips differ, +2 local)  goblins (not a repo)
origin drift  fuz_blog (ryanatkn/fuz_blog)  kit (https://codeberg.org/someone/kit)
              hint: git -C <dir> remote set-url origin <url> (each under --verbose)
sync would    move test262:main
clean 0 · on branches 0 · pinned 0      ~/dev/repos.toml · fetched 3h ago
"
        );
        let blog_block = render_entry(&r.entries[0], PATHS);
        assert!(
            blog_block.contains(
                "  needs     origin is git@github.com:ryanatkn/fuz_blog — git -C ~/dev/fuz_blog \
                 remote set-url origin git@github.com:fuzdev/fuz_blog\n"
            ),
            "{blog_block}"
        );
        let goblins_block = render_entry(&r.entries[3], PATHS);
        assert!(
            goblins_block.contains("  needs     not a repo: empty directory\n"),
            "{goblins_block}"
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
            ),
            branch("feature-x", None, Relation::Untracked, 0),
        ];
        e.branches[0].worktree = Some("/home/me/dev/gro".into());
        e.checkouts[0].uncommitted.unstaged = 1;
        e.stashes = 1;
        assert_eq!(
            render_entry(&e, PATHS),
            "\
gro  repo · owned · public · ci · follow main
  url       https://github.com/me/gro
  state     fetched 3h ago · stashes 1
  checkout  ~/dev/gro on main · 1 unstaged
  branch    main       origin/main  ahead 1 · 1 unique · 2d · checked out
  branch    feature-x  -            untracked · 2d
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
        assert_eq!(PATHS.show("/home/me/dev"), "~/dev");
        assert_eq!(PATHS.show("/home/me"), "~");
        assert_eq!(PATHS.show("/home/meadow/x"), "/home/meadow/x");
        assert_eq!(Paths { home: None }.show("/x"), "/x");
    }
}
