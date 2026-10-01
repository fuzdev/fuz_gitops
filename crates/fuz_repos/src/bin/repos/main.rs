//! `repos` — git state over the repos a `repos.toml` registry declares.
//!
//! Exit codes: `0` when the command ran (what the report says is data, not
//! failure); `1` for a runtime failure, under `sync` for anything that
//! failed — a fetch (git's failure, or the tool's refusal to run one whose
//! refspec it can't confine: the entry went unsynced and a person must
//! act), a probe, or an action git refused — and under `push` for any
//! target whose branch didn't end in sync with its upstream (held, not
//! ahead, a person's, no upstream, a remote branch in the way, detached,
//! unread, or a push that failed), as `git push` exits on a rejected ref;
//! `2` when the caller must change something — usage, a missing or invalid
//! registry, git missing or too old, an unknown target, and under `push`
//! the cwd in no entry's checkout, a third-party or pinned target, or
//! `--new-branch` in an agent's shell.
//!
//! A fatal error prints `error: …` and `hint: …` on stderr; under `--json`
//! it also prints one `ErrorReport` document on stdout, in place of the
//! report. An argument the parser rejects is reported before `--json` is
//! known, so it stays argh's text on stderr (exit 2) under `--json` too; so
//! does a non-UTF-8 argument.
//!
//! Busy detection reads the live Claude Code sessions recorded under
//! `CLAUDE_CONFIG_DIR` and `~/.claude`, excluding the calling one
//! (`CLAUDE_PID`, when it's an ancestor of this process). Under
//! `CLAUDECODE` (an agent's shell) `sync` and `push` run as a person's —
//! but for `push --new-branch`, creating a remote branch, which is the
//! user's and refused there.
//!
//! The text summary wraps at `COLUMNS` (100 when unset or under 40), piped
//! or not, and colors its group labels only when stdout is a terminal and
//! `NO_COLOR` is unset or empty.
//!
//! `status --brief [<path>]` is the `SessionStart` nudge: at most one plain
//! line on the checkout holding the path (default: the cwd), from local
//! refs, probing that entry alone. It never fails its hook — every runtime
//! condition (no registry, the path in no entry, git missing, a failed
//! probe) exits `0` in silence — and only a flag it can't take, or a
//! second path, is a usage error (exit `2`, plain text on stderr, as
//! argh's own are).

mod render;

use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argh::{EarlyExit, FromArgs};
use fuz_repos::discover::Locate;
use fuz_repos::error::{Error, Result};
use fuz_repos::git::Git;
use fuz_repos::push::{PushReportOptions, push_report};
use fuz_repos::report::ErrorReport;
use fuz_repos::sessions::{Caller, SessionsSource};
use fuz_repos::status::{
    EntryTiming, Reported, RunTimings, StatusReportOptions, checkout_status, status_report,
};
use fuz_repos::sync::{SyncReportOptions, sync_report};
use fuz_repos::{PUSH_FORMAT_VERSION, STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};

use crate::render::{
    View, render_brief, render_entry, render_push_summary, render_summary, render_sync_summary,
    render_unregistered, summary_width, use_color,
};

/// How many entries `status`, `sync`, and `push` work on at once by default.
const DEFAULT_JOBS: usize = 16;

/// The build's identity: the crate version, and the commit the binary was
/// built from (stamped by `build.rs`).
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("REPOS_BUILD"), ")");

/// `repos --version`'s line: the build's identity, then the version of each
/// `--json` document the binary prints, so a consumer can tell which shape
/// it will get — `repos <crate> (<commit>) · formats: status <n>, sync <n>,
/// push <n>`.
fn version_line() -> String {
    format!(
        "repos {VERSION} · formats: status {STATUS_FORMAT_VERSION}, sync {SYNC_FORMAT_VERSION}, \
         push {PUSH_FORMAT_VERSION}\n"
    )
}

/// repos — git state over the repos a repos.toml registry declares.
#[derive(FromArgs, Debug)]
struct Cli {
    /// path to the registry (default: the first repos.toml in the cwd or a
    /// parent; found in a checkout, the nearest parent above it holding that
    /// same file, and refused in a checkout of one of its entries)
    #[argh(option)]
    registry: Option<String>,
    /// the workspace root entry dirs resolve against (default: the dir
    /// holding the registry as found)
    #[argh(option)]
    root: Option<String>,
    /// print the version, the commit this binary was built from, and the
    /// version of each --json document
    #[argh(switch)]
    version: bool,
    #[argh(subcommand)]
    command: Option<Command>,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
enum Command {
    Status(StatusArgs),
    Sync(SyncArgs),
    Push(PushArgs),
}

/// Report every entry's git state from local refs, grouped by what to do next.
// A flat bundle of CLI switches, not domain state.
#[allow(clippy::struct_excessive_bools)]
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "status")]
struct StatusArgs {
    /// registry keys, dir names, or paths inside checkouts (default: every
    /// entry)
    #[argh(positional)]
    targets: Vec<String>,
    /// fetch owned, non-pinned entries (and third-party references named or
    /// under --references) from origin first (writes remote-tracking refs),
    /// and check that repos declared private aren't anonymously readable
    #[argh(switch)]
    fetch: bool,
    /// preview refreshing every third-party reference, as sync --references
    /// would; takes no targets (named ones are previewed so anyway)
    #[argh(switch)]
    references: bool,
    /// print the report as JSON
    #[argh(switch)]
    json: bool,
    /// add stash counts, each dirty checkout's uncommitted split, and a block
    /// per entry and per unregistered dir
    #[argh(switch)]
    verbose: bool,
    /// entries probed at once
    #[argh(option, default = "DEFAULT_JOBS")]
    jobs: usize,
    /// print wall time per phase, git spawns, and the slowest entries to
    /// stderr
    #[argh(switch)]
    timings: bool,
    /// print at most one line on the checkout holding the one target, a
    /// path (default: the cwd) — another live session working there, an
    /// operation in progress, its branch behind or ahead of origin — or
    /// nothing; for a session-start hook, so it never fails: anything but a
    /// usage error exits 0 in silence
    #[argh(switch)]
    brief: bool,
}

/// Fetch, then fast-forward each branch behind, move each stale shallow one,
/// push each one ahead, and clone each missing entry, where safe; report
/// what was done and what was held. Never force-pushes, merges, rebases, or
/// deletes. Third-party references are left as they are unless named or under
/// --references; pins, and references whose origin isn't the registry's
/// repo, always are.
// A flat bundle of CLI switches, not domain state.
#[allow(clippy::struct_excessive_bools)]
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "sync")]
struct SyncArgs {
    /// registry keys, dir names, or paths inside checkouts (default: every
    /// entry)
    #[argh(positional)]
    targets: Vec<String>,
    /// refresh every third-party reference too: fetch it over HTTPS, then
    /// fast-forward or move it where clean; takes no targets (named ones are
    /// refreshed anyway)
    #[argh(switch)]
    references: bool,
    /// print the report as JSON
    #[argh(switch)]
    json: bool,
    /// add a block per entry: the state sync acted on
    #[argh(switch)]
    verbose: bool,
    /// entries fetched, and repos acted on, at once
    #[argh(option, default = "DEFAULT_JOBS")]
    jobs: usize,
    /// print wall time per phase, git spawns, and the slowest entries to
    /// stderr
    #[argh(switch)]
    timings: bool,
}

/// Push the branch checked out where you are (or in each target's
/// checkout) to its upstream on origin: fetch, then push a branch ahead as
/// a fast-forward of exactly what was fetched, to the registry's repo over
/// SSH. Never force-pushes, pushes a tag, or touches another branch, and
/// creates a remote branch only under --new-branch (the user's); a checkout
/// another live session works in, and origin drift, hold it. Exits 0 when
/// every branch ends in sync with its upstream (pushed, created, or already
/// there), 1 when any didn't push (held, behind, diverged, detached, no
/// upstream, a remote branch in the way, a failed fetch or push), 2 for
/// usage (an unknown target, the cwd in no entry's checkout, a third-party
/// or pinned target, --new-branch in an agent's shell).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "push")]
struct PushArgs {
    /// registry keys or dir names (the entry's own checkout), or paths
    /// inside checkouts (the checkout holding each, a linked worktree's
    /// own); default: the checkout holding the cwd
    #[argh(positional)]
    targets: Vec<String>,
    /// create the branch on origin, under its own name, when it has no
    /// upstream there (none set, or a same-named one deleted on origin),
    /// never over a branch origin has, and set its upstream as git push -u
    /// does; a branch with an upstream on origin pushes as without it. The
    /// user's: refused in an agent's shell (CLAUDECODE set)
    #[argh(switch)]
    new_branch: bool,
    /// print the report as JSON
    #[argh(switch)]
    json: bool,
    /// entries fetched at once
    #[argh(option, default = "DEFAULT_JOBS")]
    jobs: usize,
    /// print wall time per phase, git spawns, and the slowest entries to
    /// stderr
    #[argh(switch)]
    timings: bool,
}

fn main() -> ExitCode {
    let args = match utf8_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let cli = match Cli::from_args(&["repos"], &args) {
        Ok(cli) => cli,
        Err(EarlyExit { output, status }) => {
            // argh's output already ends in a newline
            return if status.is_ok() {
                print!("{output}");
                ExitCode::SUCCESS
            } else {
                eprint!("{output}");
                ExitCode::from(2)
            };
        }
    };
    if let Some(Command::Status(args)) = &cli.command
        && let Some(message) = brief_conflict(args)
    {
        eprintln!("error: {message}");
        return ExitCode::from(2);
    }
    // the version of the document `--json` prints, when it's given
    let json = match &cli.command {
        Some(Command::Status(args)) => args.json.then_some(STATUS_FORMAT_VERSION),
        Some(Command::Sync(args)) => args.json.then_some(SYNC_FORMAT_VERSION),
        Some(Command::Push(args)) => args.json.then_some(PUSH_FORMAT_VERSION),
        None => None,
    };
    let printed = match run(cli) {
        Ok(printed) => printed,
        Err(e) => {
            print_error(&e, json);
            return ExitCode::from(e.exit_code());
        }
    };
    // stdout's own failure gets no JSON document: stdout is what failed
    if let Err(e) = write_stdout(&printed.stdout) {
        print_error(&e, None);
        return ExitCode::from(e.exit_code());
    }
    eprint!("{}", printed.stderr);
    if printed.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The arguments as UTF-8, or the usage error naming the first that isn't —
/// argh parses only `str`s, so no argument can be a non-UTF-8 path.
fn utf8_args(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> std::result::Result<Vec<String>, String> {
    args.map(|arg| {
        arg.into_string().map_err(|arg| {
            format!(
                "argument `{}` is not valid UTF-8; repos takes UTF-8 arguments only \
                 (paths included)",
                arg.to_string_lossy()
            )
        })
    })
    .collect()
}

/// Prints a fatal error on stderr and, under `--json` (`json` is the
/// command's document version), its document on stdout.
fn print_error(e: &Error, json: Option<u32>) {
    eprintln!("error: {}", e.message());
    if let Some(hint) = e.hint() {
        eprintln!("hint: {hint}");
    }
    if let Some(version) = json {
        // an `ErrorReport` is strings all the way down: it always serializes
        if let Ok(mut doc) = serde_json::to_string_pretty(&ErrorReport::new(e, version)) {
            doc.push('\n');
            let _ = io::stdout().lock().write_all(doc.as_bytes());
        }
    }
}

/// What a run that produced a report prints: stdout, then stderr; and
/// whether it exits `1` for something the report says failed.
#[derive(Debug, Default)]
struct Printed {
    stdout: String,
    stderr: String,
    failed: bool,
}

fn run(cli: Cli) -> Result<Printed> {
    if cli.version {
        return Ok(Printed {
            stdout: version_line(),
            ..Printed::default()
        });
    }
    let locate = Locate {
        registry: cli.registry.as_deref().map(Path::new),
        root: cli.root.as_deref().map(Path::new),
    };
    let command = cli.command.ok_or(Error::MissingCommand)?;
    if let Command::Status(args) = &command
        && args.brief
    {
        return Ok(run_brief(locate, args));
    }
    let cx = Context::new(locate)?;
    match command {
        Command::Status(args) => run_status(&cx, &args),
        Command::Sync(args) => run_sync(&cx, &args),
        Command::Push(args) => run_push(&cx, &args),
    }
}

/// What every command runs with, read from the process once: when it
/// started (for `--timings`' total), the git runner, the cwd, where the
/// global flags say the registry and the workspace root are, where the live
/// sessions are read, and `HOME` for rendering.
struct Context<'a> {
    start: Instant,
    git: Git,
    cwd: PathBuf,
    locate: Locate<'a>,
    sessions: SessionsSource,
    home: Option<String>,
}

impl<'a> Context<'a> {
    fn new(locate: Locate<'a>) -> Result<Self> {
        let start = Instant::now();
        let cwd = std::env::current_dir().map_err(|source| Error::Io {
            context: "failed to read the current directory".into(),
            source,
        })?;
        Ok(Self {
            start,
            git: Git::new(),
            cwd,
            locate,
            sessions: SessionsSource::from_env(),
            home: std::env::var("HOME").ok(),
        })
    }

    /// How rendering sees the environment, for the text summary (under
    /// `--json` nothing renders).
    fn view(&self) -> View<'_> {
        let columns = std::env::var("COLUMNS").ok();
        View {
            home: self.home.as_deref(),
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            width: summary_width(columns.as_deref()),
            color: use_color(
                io::stdout().is_terminal(),
                std::env::var_os("NO_COLOR").as_deref(),
            ),
        }
    }
}

/// Why `status --brief` can't run as asked, when it can't: a flag that
/// changes what it prints, or more than one path. Checked right after
/// parsing, as argh's own errors are, so `--brief --json` prints no
/// document: `--brief` has none.
fn brief_conflict(args: &StatusArgs) -> Option<String> {
    if !args.brief {
        return None;
    }
    let flag = [
        (args.json, "--json"),
        (args.fetch, "--fetch"),
        (args.verbose, "--verbose"),
        (args.references, "--references"),
    ]
    .into_iter()
    .find_map(|(given, flag)| given.then_some(flag));
    if let Some(flag) = flag {
        return Some(format!("--brief takes no {flag}"));
    }
    (args.targets.len() > 1).then(|| "--brief takes one path at most".to_owned())
}

/// A report as `--json` prints it.
fn to_json(report: &impl serde::Serialize) -> Result<String> {
    let mut json = serde_json::to_string_pretty(report).map_err(|e| Error::Io {
        context: "failed to serialize the report".into(),
        source: io::Error::other(e),
    })?;
    json.push('\n');
    Ok(json)
}

/// How a command prints its report: as JSON or text, and whether its
/// timings follow on stderr.
#[derive(Debug, Clone, Copy)]
struct Output {
    json: bool,
    timings: bool,
    jobs: usize,
}

/// Prints a run's report — as JSON under `out.json`, else as `text`
/// renders it — and, under `out.timings`, the run's phases on stderr.
fn print_report<R: serde::Serialize>(
    cx: &Context<'_>,
    reported: &Reported<R>,
    out: Output,
    text: impl FnOnce(&R, View<'_>) -> String,
) -> Result<Printed> {
    let render_start = Instant::now();
    let stdout = if out.json {
        to_json(&reported.report)?
    } else {
        text(&reported.report, cx.view())
    };
    let render = render_start.elapsed();
    let stderr = if out.timings {
        render_timings(&Timings {
            run: &reported.timings,
            render,
            total: cx.start.elapsed(),
            jobs: out.jobs,
            spawns: cx.git.spawns(),
        })
    } else {
        String::new()
    };
    Ok(Printed {
        stdout,
        stderr,
        failed: false,
    })
}

fn run_status(cx: &Context<'_>, args: &StatusArgs) -> Result<Printed> {
    let reported = status_report(
        &cx.git,
        &cx.cwd,
        cx.locate,
        &args.targets,
        StatusReportOptions {
            fetch: args.fetch,
            references: args.references,
            jobs: args.jobs,
            sessions: &cx.sessions,
        },
    )?;
    let out = Output {
        json: args.json,
        timings: args.timings,
        jobs: args.jobs,
    };
    print_report(cx, &reported, out, |report, view| {
        let mut out = String::new();
        if args.verbose {
            for e in &report.entries {
                out.push_str(&render_entry(e, Path::new(&report.workspace), view));
                out.push('\n');
            }
            for u in report.unregistered.iter().flatten() {
                out.push_str(&render_unregistered(u, report, view));
                out.push('\n');
            }
        }
        out.push_str(&render_summary(report, view, args.verbose));
        out
    })
}

fn run_sync(cx: &Context<'_>, args: &SyncArgs) -> Result<Printed> {
    let reported = sync_report(
        &cx.git,
        &cx.cwd,
        cx.locate,
        &args.targets,
        SyncReportOptions {
            references: args.references,
            jobs: args.jobs,
            sessions: &cx.sessions,
        },
    )?;
    let out = Output {
        json: args.json,
        timings: args.timings,
        jobs: args.jobs,
    };
    let printed = print_report(cx, &reported, out, |report, view| {
        let mut out = String::new();
        if args.verbose {
            for e in &report.status.entries {
                out.push_str(&render_entry(e, Path::new(&report.status.workspace), view));
                out.push('\n');
            }
        }
        out.push_str(&render_sync_summary(report, view, args.verbose));
        out
    })?;
    Ok(Printed {
        failed: reported.report.failed(),
        ..printed
    })
}

/// `repos push`: fails (exit `1`) unless every target's branch ends in
/// sync (`push_report`).
fn run_push(cx: &Context<'_>, args: &PushArgs) -> Result<Printed> {
    let reported = push_report(
        &cx.git,
        &cx.cwd,
        cx.locate,
        &args.targets,
        PushReportOptions {
            new_branch: args.new_branch,
            caller: Caller::from_env(),
            jobs: args.jobs,
            sessions: &cx.sessions,
        },
    )?;
    let out = Output {
        json: args.json,
        timings: args.timings,
        jobs: args.jobs,
    };
    let printed = print_report(cx, &reported, out, |report, view| {
        render_push_summary(report, view)
    })?;
    Ok(Printed {
        // as `git push` on a rejected ref: a branch isn't where it was asked
        failed: !reported.report.in_sync(),
        ..printed
    })
}

/// `status --brief`: its line, or nothing — every error is silence, a
/// refused root included (`checkout_status` says what it probes).
fn run_brief(locate: Locate<'_>, args: &StatusArgs) -> Printed {
    let path = args.targets.first().map_or(".", String::as_str);
    let Ok(cx) = Context::new(locate) else {
        return Printed::default();
    };
    let Ok(Some(found)) =
        checkout_status(&cx.git, &cx.cwd, Path::new(path), cx.locate, &cx.sessions)
    else {
        return Printed::default();
    };
    let render_start = Instant::now();
    let entry = &found.report.entry;
    // a worktree the probe couldn't read: nothing to say of it
    let Some(checkout) = entry.checkout_at(&found.report.checkout) else {
        return Printed::default();
    };
    // one plain line, whatever the terminal
    let view = View {
        color: false,
        ..cx.view()
    };
    let line = render_brief(entry, checkout, view);
    let render = render_start.elapsed();
    let mut printed = Printed {
        stdout: line.unwrap_or_default(),
        ..Printed::default()
    };
    if args.timings {
        printed.stderr = render_timings(&Timings {
            run: &found.timings,
            render,
            total: cx.start.elapsed(),
            jobs: 1,
            spawns: cx.git.spawns(),
        });
    }
    printed
}

/// Writes to stdout, treating a closed pipe (`repos status | head`) as done.
fn write_stdout(s: &str) -> Result<()> {
    match io::stdout().lock().write_all(s.as_bytes()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(Error::Io {
            context: "failed to write to stdout".into(),
            source: e,
        }),
        _ => Ok(()),
    }
}

/// What `--timings` prints: the run's phases (`RunTimings`), then rendering,
/// the total, and the git spawns.
#[derive(Debug)]
struct Timings<'a> {
    run: &'a RunTimings,
    render: Duration,
    total: Duration,
    jobs: usize,
    spawns: u32,
}

/// How many of the slowest entries `--timings` names.
const SLOWEST: usize = 6;

fn render_timings(t: &Timings<'_>) -> String {
    let run = t.run;
    let ms = |d: Duration| format!("{}ms", d.as_millis());
    let fetched = run.entries.iter().any(|e| !e.fetch.is_zero());
    let phase = if fetched { "fetch + probe" } else { "probe" };
    let scan = run
        .scan
        .map(|d| format!(" · scan {}", ms(d)))
        .unwrap_or_default();
    let act = run
        .act
        .map(|d| format!(" · act {}", ms(d)))
        .unwrap_or_default();
    let mut out = format!(
        "timings   load {} · {phase} {} (jobs {}){act}{scan} · render {} · total {}\n",
        ms(run.load),
        ms(run.probe),
        t.jobs,
        ms(t.render),
        ms(t.total),
    );
    let probe_sum: Duration = run.entries.iter().map(|e| e.probe).sum();
    let _ = writeln!(
        out,
        "git       {} spawns over {} entries · probe time summed {}",
        t.spawns,
        run.entries.len(),
        ms(probe_sum)
    );
    let slowest = |pick: fn(&EntryTiming) -> Duration| {
        let mut sorted: Vec<_> = run.entries.iter().filter(|e| !pick(e).is_zero()).collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(pick(e)));
        sorted
            .iter()
            .take(SLOWEST)
            .map(|e| format!("{} {}", e.key, ms(pick(e))))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let _ = writeln!(out, "slowest   probe: {}", slowest(|e| e.probe));
    if fetched {
        let fetch_sum: Duration = run.entries.iter().map(|e| e.fetch).sum();
        let _ = writeln!(
            out,
            "          fetch: {} (summed {})",
            slowest(|e| e.fetch),
            ms(fetch_sum)
        );
    }
    if run.entries.iter().any(|e| !e.visibility.is_zero()) {
        let _ = writeln!(out, "          visibility: {}", slowest(|e| e.visibility));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_is_a_usage_error() {
        let cli = Cli::from_args(&["repos"], &[]).unwrap();
        let e = run(cli).unwrap_err();
        assert!(matches!(e, Error::MissingCommand), "{e}");
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn version_needs_no_subcommand() {
        let cli = Cli::from_args(&["repos"], &["--version"]).unwrap();
        assert!(cli.version && cli.command.is_none());
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")));
    }
}
