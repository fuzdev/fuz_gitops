//! `repos` — git state over the repos a `repos.toml` registry declares.
//!
//! Exit codes: `0` when the command ran (what the report says is data, not
//! failure); `1` for a runtime failure, and under `sync` for anything that
//! failed — a fetch (git's failure, or the tool's refusal to run one whose
//! refspec it can't confine: the entry went unsynced and a person must
//! act), a probe, or an action git refused; `2` when the caller must change
//! something — usage, a missing or invalid registry, git missing or too
//! old, an unknown target.
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
//! `CLAUDECODE` (an agent's shell) every push is held for the gateway.
//!
//! The text summary wraps at `COLUMNS` (100 when unset or under 40), piped
//! or not, and colors its group labels only when stdout is a terminal and
//! `NO_COLOR` is unset or empty.

mod render;

use std::fmt::Write as _;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argh::{EarlyExit, FromArgs};
use fuz_repos::classify::Refresh;
use fuz_repos::clone::CLONE_TIMEOUT;
use fuz_repos::discover::{RegistryLocation, find_registry, resolve_targets};
use fuz_repos::error::{Error, Result};
use fuz_repos::git::Git;
use fuz_repos::probe::RegistryDirs;
use fuz_repos::registry::{Entry, ValidRegistry};
use fuz_repos::report::{ErrorReport, StatusReport, SyncReport};
use fuz_repos::scan::{Scan, scan_unregistered};
use fuz_repos::sessions::{Caller, SessionsSource, read_live_sessions};
use fuz_repos::status::{EntryTiming, StatusOptions, mark_moved_worktrees, status};
use fuz_repos::sync::{SyncOptions, sync};
use fuz_repos::{STATUS_FORMAT_VERSION, SYNC_FORMAT_VERSION};

use crate::render::{
    View, render_entry, render_summary, render_sync_summary, render_unregistered, summary_width,
    use_color,
};

/// The build's identity: the crate version, and the commit the binary was
/// built from (stamped by `build.rs`).
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("REPOS_BUILD"), ")");

/// repos — git state over the repos a repos.toml registry declares.
#[derive(FromArgs, Debug)]
struct Cli {
    /// path to the registry (default: the first repos.toml in the cwd or a
    /// parent)
    #[argh(option)]
    registry: Option<String>,
    /// the workspace root entry dirs resolve against (default: the dir
    /// holding the registry as found)
    #[argh(option)]
    root: Option<String>,
    /// print the version and the commit this binary was built from
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
    #[argh(option, default = "16")]
    jobs: usize,
    /// print wall time per phase, git spawns, and the slowest entries to
    /// stderr
    #[argh(switch)]
    timings: bool,
}

/// Fetch, then fast-forward each branch behind, move each stale shallow one,
/// push each one ahead, and clone each missing entry, where safe; report
/// what was done and what was held. Never force-pushes, merges, rebases, or
/// deletes; an agent's pushes are held. Third-party references are left as
/// they are unless named or under --references; pins, and references whose
/// origin isn't the registry's repo, always are.
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
    #[argh(option, default = "16")]
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
    // the version of the document `--json` prints, when it's given
    let json = match &cli.command {
        Some(Command::Status(args)) => args.json.then_some(STATUS_FORMAT_VERSION),
        Some(Command::Sync(args)) => args.json.then_some(SYNC_FORMAT_VERSION),
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
            stdout: format!("repos {VERSION}\n"),
            ..Printed::default()
        });
    }
    let locate = Locate {
        registry: cli.registry.as_deref().map(Path::new),
        root: cli.root.as_deref().map(Path::new),
    };
    match cli.command {
        Some(Command::Status(args)) => run_status(locate, &args),
        Some(Command::Sync(args)) => run_sync(locate, &args),
        None => Err(Error::MissingCommand),
    }
}

/// Where the global flags say the registry and the workspace root are.
#[derive(Debug, Clone, Copy)]
struct Locate<'a> {
    registry: Option<&'a Path>,
    root: Option<&'a Path>,
}

/// The registry found and validated, and the entries the targets name.
struct Loaded {
    git: Git,
    loc: RegistryLocation,
    registry: ValidRegistry,
    all: Vec<Entry>,
    entries: Vec<Entry>,
}

fn load(locate: Locate<'_>, targets: &[String]) -> Result<Loaded> {
    let cwd = std::env::current_dir().map_err(|source| Error::Io {
        context: "failed to read the current directory".into(),
        source,
    })?;
    let git = Git::new();
    // first: discovery's fallback runs git too
    git.check_version(&cwd)?;
    let loc = find_registry(&cwd, locate.registry, locate.root, &git)?;
    // validated before targets resolve and anything is probed
    let registry = ValidRegistry::load(&loc.path)?;
    let all = registry.entries();
    let entries = resolve_targets(&all, &loc.root, &cwd, targets, &git)?;
    Ok(Loaded {
        git,
        loc,
        registry,
        all,
        entries,
    })
}

/// Which references a run refreshes: the named ones — every entry of a run
/// given targets (a path inside a checkout names its entry too) — or,
/// without targets, every third-party one under `--references`. Both at
/// once is a usage error: the run would say two things.
const fn refresh_asked(targets: &[String], references: bool) -> Result<Refresh> {
    match (targets.is_empty(), references) {
        (false, true) => Err(Error::ReferencesWithTargets),
        (false, false) => Ok(Refresh::Named),
        (true, true) => Ok(Refresh::References),
        (true, false) => Ok(Refresh::Unasked),
    }
}

/// The unregistered scan over the whole workspace: without targets, and
/// with them when a named entry's dir is missing — a clone the scan finds
/// already made under another name holds its clone. `None` when it didn't
/// run. Only a run without targets reports what it found: with them the
/// report is about the named entries.
fn scan_workspace(loaded: &Loaded, targets: &[String]) -> Result<Option<Scan>> {
    // missing as the probe reads it: nothing at the path, not even a link
    let missing = |e: &Entry| {
        std::fs::symlink_metadata(loaded.loc.root.join(&e.dir))
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
    };
    if !targets.is_empty() && !loaded.entries.iter().any(missing) {
        return Ok(None);
    }
    scan_unregistered(
        &loaded.loc.root,
        &loaded.all,
        loaded.registry.owners(),
        &loaded.git,
    )
    .map(Some)
    .map_err(|source| Error::Io {
        context: format!(
            "failed to list the workspace root {}",
            loaded.loc.root.display()
        ),
        source,
    })
}

/// How rendering sees the environment, for a run printing JSON or not.
fn view(home: Option<&str>, json: bool) -> View<'_> {
    let columns = std::env::var("COLUMNS").ok();
    View {
        home,
        now: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        width: summary_width(columns.as_deref()),
        // never under `--json`, which renders nothing
        color: !json
            && use_color(
                io::stdout().is_terminal(),
                std::env::var_os("NO_COLOR").as_deref(),
            ),
    }
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

fn run_sync(locate: Locate<'_>, args: &SyncArgs) -> Result<Printed> {
    let start = Instant::now();
    let refresh = refresh_asked(&args.targets, args.references)?;
    let loaded = load(locate, &args.targets)?;
    let load_time = start.elapsed();
    // before anything is cloned: a missing entry cloned under another name
    // holds its clone
    let scan_start = Instant::now();
    let scan = scan_workspace(&loaded, &args.targets)?;
    let scan_time = scan.as_ref().map(|_| scan_start.elapsed());
    let Loaded {
        git,
        loc,
        all,
        entries,
        ..
    } = loaded;

    let source = SessionsSource::from_env();
    let read_live = || read_live_sessions(&source);
    let run = sync(
        &entries,
        &RegistryDirs::new(&loc.root, &all),
        &loc.root,
        &git,
        SyncOptions {
            jobs: args.jobs,
            visibility_base: None,
            read_live: &read_live,
            caller: Caller::from_env(),
            clone_timeout: CLONE_TIMEOUT,
            refresh,
            unregistered: scan.as_ref().map(|s| &s.unregistered[..]),
        },
    );
    let mut status = StatusReport::new(
        loc.root.to_string_lossy().into_owned(),
        loc.path.to_string_lossy().into_owned(),
        true,
        run.sessions,
        run.entries,
    );
    if let Some(scan) = scan.filter(|_| args.targets.is_empty()) {
        // before render: a gone worktree the scan found moved gets no command
        mark_moved_worktrees(&mut status.entries, &scan);
        status.unregistered = Some(scan.unregistered);
    }
    let report = SyncReport::new(status, run.outcomes);

    let render_start = Instant::now();
    let home = std::env::var("HOME").ok();
    let view = view(home.as_deref(), args.json);
    let stdout = if args.json {
        to_json(&report)?
    } else {
        let mut out = String::new();
        if args.verbose {
            for e in &report.status.entries {
                out.push_str(&render_entry(e, &loc.root, view));
                out.push('\n');
            }
        }
        out.push_str(&render_sync_summary(&report, view, args.verbose));
        out
    };
    let render_time = render_start.elapsed();

    let mut printed = Printed {
        stdout,
        stderr: String::new(),
        failed: report.failed(),
    };
    if args.timings {
        printed.stderr = render_timings(&Timings {
            load: load_time,
            probe: run.probe_elapsed,
            act: Some(run.act_elapsed),
            scan: scan_time,
            render: render_time,
            total: start.elapsed(),
            jobs: args.jobs,
            spawns: git.spawns(),
            entries: &run.timings,
        });
    }
    Ok(printed)
}

fn run_status(locate: Locate<'_>, args: &StatusArgs) -> Result<Printed> {
    let start = Instant::now();
    let refresh = refresh_asked(&args.targets, args.references)?;
    let loaded = load(locate, &args.targets)?;
    let load_time = start.elapsed();
    // first, since a missing entry cloned under another name holds its
    // clone; reported without targets only (`scan_workspace`)
    let scan_start = Instant::now();
    let scan = scan_workspace(&loaded, &args.targets)?;
    let scan_time = scan.as_ref().map(|_| scan_start.elapsed());
    let Loaded {
        git,
        loc,
        all,
        entries,
        ..
    } = loaded;

    let live = read_live_sessions(&SessionsSource::from_env());
    let run = status(
        &entries,
        &RegistryDirs::new(&loc.root, &all),
        &loc.root,
        &git,
        StatusOptions {
            fetch: args.fetch,
            refresh,
            unregistered: scan.as_ref().map(|s| &s.unregistered[..]),
            jobs: args.jobs,
            visibility_base: None,
            live: &live,
            caller: Caller::from_env(),
        },
    );
    let mut report = StatusReport::new(
        loc.root.to_string_lossy().into_owned(),
        loc.path.to_string_lossy().into_owned(),
        args.fetch,
        run.sessions,
        run.entries,
    );
    if let Some(scan) = scan.filter(|_| args.targets.is_empty()) {
        // before render: a gone worktree the scan found moved gets no command
        mark_moved_worktrees(&mut report.entries, &scan);
        report.unregistered = Some(scan.unregistered);
    }

    let render_start = Instant::now();
    let home = std::env::var("HOME").ok();
    let view = view(home.as_deref(), args.json);
    let out = if args.json {
        to_json(&report)?
    } else {
        let mut out = String::new();
        if args.verbose {
            for e in &report.entries {
                out.push_str(&render_entry(e, &loc.root, view));
                out.push('\n');
            }
            for u in report.unregistered.iter().flatten() {
                out.push_str(&render_unregistered(u, &report, view));
                out.push('\n');
            }
        }
        out.push_str(&render_summary(&report, view, args.verbose));
        out
    };
    let render_time = render_start.elapsed();

    let mut printed = Printed {
        stdout: out,
        ..Printed::default()
    };
    if args.timings {
        printed.stderr = render_timings(&Timings {
            load: load_time,
            probe: run.elapsed,
            act: None,
            scan: scan_time,
            render: render_time,
            total: start.elapsed(),
            jobs: args.jobs,
            spawns: git.spawns(),
            entries: &run.timings,
        });
    }
    Ok(printed)
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

#[derive(Debug)]
struct Timings<'a> {
    load: Duration,
    probe: Duration,
    /// Sync's acting; `None` for `status`.
    act: Option<Duration>,
    /// `None` when the unregistered scan didn't run.
    scan: Option<Duration>,
    render: Duration,
    total: Duration,
    jobs: usize,
    spawns: u32,
    entries: &'a [EntryTiming],
}

/// How many of the slowest entries `--timings` names.
const SLOWEST: usize = 6;

fn render_timings(t: &Timings<'_>) -> String {
    let ms = |d: Duration| format!("{}ms", d.as_millis());
    let fetched = t.entries.iter().any(|e| !e.fetch.is_zero());
    let phase = if fetched { "fetch + probe" } else { "probe" };
    let scan = t
        .scan
        .map(|d| format!(" · scan {}", ms(d)))
        .unwrap_or_default();
    let act = t
        .act
        .map(|d| format!(" · act {}", ms(d)))
        .unwrap_or_default();
    let mut out = format!(
        "timings   load {} · {phase} {} (jobs {}){act}{scan} · render {} · total {}\n",
        ms(t.load),
        ms(t.probe),
        t.jobs,
        ms(t.render),
        ms(t.total),
    );
    let probe_sum: Duration = t.entries.iter().map(|e| e.probe).sum();
    let _ = writeln!(
        out,
        "git       {} spawns over {} entries · probe time summed {}",
        t.spawns,
        t.entries.len(),
        ms(probe_sum)
    );
    let slowest = |pick: fn(&EntryTiming) -> Duration| {
        let mut sorted: Vec<_> = t.entries.iter().filter(|e| !pick(e).is_zero()).collect();
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
        let fetch_sum: Duration = t.entries.iter().map(|e| e.fetch).sum();
        let _ = writeln!(
            out,
            "          fetch: {} (summed {})",
            slowest(|e| e.fetch),
            ms(fetch_sum)
        );
    }
    if t.entries.iter().any(|e| !e.visibility.is_zero()) {
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
