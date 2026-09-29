//! `repos` — git state over the repos a `repos.toml` registry declares.
//!
//! Exit codes: `0` when the command ran (what the report says is data, not
//! failure); `1` for a runtime failure; `2` when the caller must change
//! something — usage, a missing or invalid registry, git missing or too old,
//! an unknown target.
//!
//! A fatal error prints `error: …` and `hint: …` on stderr; under `status
//! --json` it also prints one `ErrorReport` document on stdout, in place of
//! the report. An argument the parser rejects is reported before `--json` is
//! known, so it stays argh's text on stderr (exit 2) under `--json` too; so
//! does a non-UTF-8 argument.

mod render;

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argh::{EarlyExit, FromArgs};
use fuz_repos::discover::{find_registry, resolve_targets};
use fuz_repos::error::{Error, Result};
use fuz_repos::git::Git;
use fuz_repos::probe::RegistryDirs;
use fuz_repos::registry::ValidRegistry;
use fuz_repos::report::{ErrorReport, StatusReport};
use fuz_repos::scan::scan_unregistered;
use fuz_repos::status::{EntryTiming, StatusOptions, mark_moved_worktrees, status};

use crate::render::{View, render_entry, render_summary, render_unregistered};

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
    /// fetch owned, non-pinned entries from origin first (writes
    /// remote-tracking refs)
    #[argh(switch)]
    fetch: bool,
    /// print the report as JSON
    #[argh(switch)]
    json: bool,
    /// add stash counts, the uncommitted split, and a block per entry and per
    /// unregistered dir
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
    let json = matches!(&cli.command, Some(Command::Status(args)) if args.json);
    let printed = match run(cli) {
        Ok(printed) => printed,
        Err(e) => {
            print_error(&e, json);
            return ExitCode::from(e.exit_code());
        }
    };
    // stdout's own failure gets no JSON document: stdout is what failed
    if let Err(e) = write_stdout(&printed.stdout) {
        print_error(&e, false);
        return ExitCode::from(e.exit_code());
    }
    eprint!("{}", printed.stderr);
    ExitCode::SUCCESS
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

/// Prints a fatal error on stderr and, under `--json`, its document on
/// stdout.
fn print_error(e: &Error, json: bool) {
    eprintln!("error: {}", e.message());
    if let Some(hint) = e.hint() {
        eprintln!("hint: {hint}");
    }
    if json {
        // an `ErrorReport` is strings all the way down: it always serializes
        if let Ok(mut doc) = serde_json::to_string_pretty(&ErrorReport::new(e)) {
            doc.push('\n');
            let _ = io::stdout().lock().write_all(doc.as_bytes());
        }
    }
}

/// What a successful run prints: stdout, then stderr.
#[derive(Debug, Default)]
struct Printed {
    stdout: String,
    stderr: String,
}

fn run(cli: Cli) -> Result<Printed> {
    if cli.version {
        return Ok(Printed {
            stdout: format!("repos {VERSION}\n"),
            stderr: String::new(),
        });
    }
    let locate = Locate {
        registry: cli.registry.as_deref().map(Path::new),
        root: cli.root.as_deref().map(Path::new),
    };
    match cli.command {
        Some(Command::Status(args)) => run_status(locate, &args),
        None => Err(Error::MissingCommand),
    }
}

/// Where the global flags say the registry and the workspace root are.
#[derive(Debug, Clone, Copy)]
struct Locate<'a> {
    registry: Option<&'a Path>,
    root: Option<&'a Path>,
}

fn run_status(locate: Locate<'_>, args: &StatusArgs) -> Result<Printed> {
    let start = Instant::now();
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
    let entries = resolve_targets(&all, &loc.root, &cwd, &args.targets, &git)?;
    let load_time = start.elapsed();

    let run = status(
        &entries,
        &RegistryDirs::new(&loc.root, &all),
        &loc.root,
        &git,
        StatusOptions {
            fetch: args.fetch,
            jobs: args.jobs,
        },
    );
    let mut report = StatusReport::new(
        loc.root.to_string_lossy().into_owned(),
        loc.path.to_string_lossy().into_owned(),
        run.entries,
    );
    // the whole workspace only: with targets the report is about the named
    // entries, and `unregistered` stays `null`
    let scan_start = Instant::now();
    let scan_time = if args.targets.is_empty() {
        let scan =
            scan_unregistered(&loc.root, &all, registry.owners(), &git).map_err(|source| {
                Error::Io {
                    context: format!("failed to list the workspace root {}", loc.root.display()),
                    source,
                }
            })?;
        // before render: a gone worktree the scan found moved gets no command
        mark_moved_worktrees(&mut report.entries, &scan);
        report.unregistered = Some(scan.unregistered);
        Some(scan_start.elapsed())
    } else {
        None
    };

    let render_start = Instant::now();
    let home = std::env::var("HOME").ok();
    let view = View {
        home: home.as_deref(),
        now: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let out = if args.json {
        let mut json = serde_json::to_string_pretty(&report).map_err(|e| Error::Io {
            context: "failed to serialize the report".into(),
            source: io::Error::other(e),
        })?;
        json.push('\n');
        json
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
        stderr: String::new(),
    };
    if args.timings {
        printed.stderr = render_timings(&Timings {
            load: load_time,
            probe: run.elapsed,
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
    let mut out = format!(
        "timings   load {} · {phase} {} (jobs {}){scan} · render {} · total {}\n",
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
