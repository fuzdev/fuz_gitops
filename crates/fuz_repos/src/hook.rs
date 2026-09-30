//! `repos hook pre-tool-use`: Claude Code's `PreToolUse` hook, which
//! denies an agent's Bash call that pushes with raw git, creates a remote
//! branch, or hides the agent from `repos push`.
//!
//! Every Bash call the hook sees is an agent's, so the policy is the agent
//! profile's. It denies a command that would run:
//!
//! - **a raw git push** — `git push` or `git send-pack` (or `git-push`,
//!   `git-send-pack`), whatever the path to git, the quoting, git's global
//!   options (`-C`, `-c`, `--git-dir`, `--work-tree`, `--namespace`,
//!   `--config-env`, `--attr-source`, and every flag), or the wrappers
//!   around it; git with a subcommand it can't know (`git $cmd`, `xargs
//!   git`); a git alias defined to push (`-c alias.p=push`, `git config
//!   alias.p push`); and git running a command (`submodule foreach`,
//!   `bisect run`, `rebase --exec`) that does. The push is the gateway's:
//!   `repos push`.
//! - **`repos push --new-branch`** — creating a remote branch is the
//!   user's (`repos` refuses it under `CLAUDECODE` too; this covers a run
//!   that hides it).
//! - **`repos push` with `CLAUDECODE` unset or emptied** — the variable is
//!   how `repos` knows an agent runs it, and what it gates is `push
//!   --new-branch`. Denied: a `CLAUDECODE=` assignment with an empty or
//!   unknown value on a `repos push` (or a `repos` whose subcommand can't
//!   be known) or on a wrapper or shell that runs one, `env -u
//!   CLAUDECODE`, `env -i`, and, anywhere in a call that also runs one,
//!   `unset CLAUDECODE` or a standalone or exported empty assignment.
//!   Setting it non-empty (still an agent), and clearing it for any other
//!   `repos` subcommand or a call that runs none, pass.
//!
//! **How a command is read** (`shell`, for the words): each simple command,
//! and each substitution's script, is an argv. Leading `NAME=value` words
//! are skipped; so are wrappers that run the rest as a command — `env`,
//! `sudo`, `doas`, `timeout`, `nice`, `ionice`, `nohup`, `time`, `command`
//! (but `command -v`), `exec`, `builtin`, `setsid`, `stdbuf`, `flock`,
//! `chrt`, `taskset`, `unbuffer`, `chronic`, `busybox`, and the shell's
//! reserved words — each with its own options. What runs a script is read
//! as one: `bash`/`sh`/`zsh`/`dash`/`ksh`/`mksh`/`ash`/`fish` with `-c`,
//! `eval`, `su -c`, `flock -c`, `env -S`, `watch`, `find -exec`, `xargs`
//! (its command gets an argument it can't know), `gro gitops_run`, and a
//! shell reading stdin from a here-document, a here-string, or a pipe (the
//! words of the commands before it in the pipeline, up to `MAX_PIPELINE`
//! back). A command name whose basename can't be known (`$g`, `$(which
//! git)`) is read as git that pushes only with a literal `push`, and as a
//! shell only with `-c`. Any other command name is looked past for the
//! first word that's git, a shell, `eval`, `repos`, or a wrapper, so an
//! unknown wrapper still reveals `git push` — but for commands whose
//! arguments are text (`echo`, `printf`, `grep`, `rg`, `man`, `which`,
//! `test`, …): a quoted string is a command only where a shell would run
//! it.
//!
//! **Fails toward a deny** where the text can't be read — an unterminated
//! quote or substitution, nesting past the reader's `MAX_NESTING`, scripts
//! and argvs nested past `MAX_DEPTH`, or more reading than `MAX_WORK` —
//! and one command of it (a
//! line, continuations joined, up to a `;`, `&`, or `|`) has a push word
//! after a git word, quoting ignored (or `repos push` and `--new-branch`, or
//! `repos push` with `CLAUDECODE` cleared): a false positive costs a
//! message. So does brace expansion past the reader's bounds, whatever the
//! words (`Script::unexpanded`): the words it leaves unexpanded, or drops
//! past its word cap, can't be known, command names included
//! (`{git,push}`), and a command that size is never an ordinary one — and
//! a pipeline into a shell longer than `MAX_PIPELINE`, whose earlier
//! commands pipe in text it doesn't read.
//!
//! **Out of reach**, as for any reading of a command: scripts in files,
//! other languages (`python -c`, `perl -e`), aliases and functions defined
//! outside the call, git configured to push by another name
//! (`GIT_CONFIG_*`), `gh api` ref writes, and anything that runs git
//! through a path this reading doesn't follow. Guidance, not a boundary:
//! the host's branch rules are the floor.
//!
//! **Fails open** on input it can't read as the hook's: invalid JSON, a
//! tool other than Bash, or no `tool_input.command` — a schema change must
//! not wedge every Bash call, and the settings' deny rule on `git push` is
//! the backstop.

use std::collections::HashMap;

use serde_json::Value;

use crate::shell::{DYN, Script, SimpleCommand, basename, parse, split_assignment};

/// How deep scripts in scripts are read (`bash -c "eval '…'"`).
///
/// Each argv a wrapper hands on (`xargs`, `find -exec`, `env -S`, `watch
/// -x`) counts a level too; past it, the text is judged by its words alone
/// (`by_words`). It bounds how deep the reading recurses; `MAX_WORK`
/// bounds how long it takes.
pub const MAX_DEPTH: u32 = 16;

/// How much text one call's reading may take, over the scripts and argvs
/// read inside it.
///
/// The call's own text, read once in linear time, doesn't count; each
/// script and argv read inside it counts at each level that reads it. Past
/// it, nothing more is read, and the whole call is judged by its words
/// (`by_words`). Far past any real command, it bounds the time a
/// pathological one takes.
pub const MAX_WORK: usize = 1 << 21;

/// Why a Bash call is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// It runs a raw git push.
    GitPush,
    /// It couldn't be read, and names git and a push; or its brace
    /// expansion ran past the reader's bounds.
    Unreadable,
    /// It runs `repos push --new-branch`.
    NewBranch,
    /// It runs `repos push` with `CLAUDECODE` unset or emptied.
    Claudecode,
}

impl Denial {
    /// What the agent is told.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::GitPush => {
                "raw git push is the gateway's: run `repos push` (pushes the branch checked \
                 out here, fast-forward only; `repos push <key>` for another checkout). \
                 Creating a remote branch is the user's (`repos push --new-branch`). To update \
                 a local bare repo, fetch into it: `git -C <bare> fetch <src> <branch>:<branch>` \
                 (`<src>` a path from the bare repo, or absolute)."
            }
            Self::Unreadable => {
                "this command couldn't be read as shell (an unterminated quote or \
                 substitution, nesting too deep, or too long to read), and one of its commands \
                 has git and push in it — or its brace expansion is too large to follow — so \
                 it's held as a raw git push: push with `repos push` (the branch checked out \
                 here; `repos push <key>` for another checkout). If it isn't a push, fix its \
                 quoting or split it up."
            }
            Self::NewBranch => {
                "creating a remote branch is the user's: `repos push --new-branch` is theirs to \
                 run — ask them, or push a branch that already has an upstream with `repos push`."
            }
            Self::Claudecode => {
                "CLAUDECODE tells repos an agent runs it: run repos push with the environment \
                 as it is, never with CLAUDECODE unset or emptied."
            }
        }
    }

    /// The hook's JSON on stdout, which Claude Code reads the deny from.
    pub fn hook_output(self) -> String {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": self.reason(),
            }
        })
        .to_string()
    }
}

/// Reads a `PreToolUse` hook's input: a Bash call's command is checked
/// (`check_command`), and anything else — another tool, input that isn't
/// the hook's JSON — has no opinion.
pub fn check_pre_tool_use(input: &[u8]) -> Option<Denial> {
    let input: Value = serde_json::from_slice(input).ok()?;
    if input.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let command = input.get("tool_input")?.get("command")?.as_str()?;
    check_command(command)
}

/// What denies `command`, a Bash call's shell text, if anything.
pub fn check_command(command: &str) -> Option<Denial> {
    let mut scan = Scan::default();
    scan.script(command, 0, false);
    if scan.exhausted {
        scan.by_words(command, false);
    }
    scan.denial()
}

/// Shells, which run a script under `-c` and read one from stdin without
/// a file.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

/// Commands whose arguments are text, never a command: looked no further
/// into.
const TEXT_COMMANDS: &[&str] = &[
    "echo", "printf", "grep", "egrep", "fgrep", "rg", "ag", "ack", "man", "info", "help", "which",
    "type", "whatis", "whereis", "apropos", ":", "true", "false", "test", "[", "[[", "alias",
];

/// Shell words that run no command of their own and start the one after.
const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "fi", "done", "esac", "coproc",
];

/// Shell words whose following words aren't a command.
const RESERVED_DATA: &[&str] = &["for", "case", "select", "function", "in"];

/// Options that take the next word, by wrapper.
const SUDO_VALUES: &[&str] = &[
    "-u",
    "-g",
    "-C",
    "-D",
    "-h",
    "-p",
    "-r",
    "-t",
    "-T",
    "-U",
    "--user",
    "--group",
    "--close-from",
    "--chdir",
    "--host",
    "--prompt",
    "--role",
    "--type",
    "--command-timeout",
    "--other-user",
];
const XARGS_VALUES: &[&str] = &[
    "-a",
    "-d",
    "-E",
    "-L",
    "-n",
    "-P",
    "-s",
    "--arg-file",
    "--delimiter",
    "--eof",
    "--max-lines",
    "--max-args",
    "--max-procs",
    "--max-chars",
    "--process-slot-var",
];

/// The text on a command's stdin the script holds, when asked.
type Stdin<'a> = &'a dyn Fn() -> Piped;

const fn no_stdin() -> Piped {
    Piped {
        texts: Vec::new(),
        cut: false,
    }
}

/// The text on a command's stdin the script holds (`stdin_of`).
struct Piped {
    texts: Vec<String>,
    /// The pipeline feeding it runs past `MAX_PIPELINE`: text from the
    /// commands further back reaches it unread.
    cut: bool,
}

/// How many commands back a pipeline is read for a shell's stdin.
const MAX_PIPELINE: usize = 16;

/// What a wrapper leaves to run.
enum Next {
    /// The command starting this many words past the wrapper's name.
    At(usize),
    /// This argv instead.
    Argv(Vec<String>),
    /// This script.
    Script(String),
    /// Nothing it runs is read.
    Stop,
}

// Findings, each set on its own as the call is read, not a state.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
struct Scan {
    push: bool,
    maybe_push: bool,
    new_branch: bool,
    /// `repos push` runs, or `repos` with a subcommand that can't be known.
    runs_repos_push: bool,
    /// `CLAUDECODE` cleared for the rest of the call (`unset`, a standalone
    /// assignment).
    clears_claudecode: bool,
    /// `repos push` run with `CLAUDECODE` cleared in its own environment.
    repos_without_claudecode: bool,
    /// Script text the call writes to a file, by path (`script_path`):
    /// what `cat` or `tee` writes from a here-document or here-string, or
    /// `echo` or `printf` writes, read if the call runs that file.
    written: HashMap<String, Vec<String>>,
    /// How much text the scripts and argvs read so far held.
    work: usize,
    /// `work` passed `MAX_WORK`: nothing more is read, and the whole call
    /// is judged by its words.
    exhausted: bool,
}

impl Scan {
    const fn denial(&self) -> Option<Denial> {
        if self.push {
            Some(Denial::GitPush)
        } else if self.maybe_push {
            Some(Denial::Unreadable)
        } else if self.new_branch {
            Some(Denial::NewBranch)
        } else if self.repos_without_claudecode || (self.clears_claudecode && self.runs_repos_push)
        {
            Some(Denial::Claudecode)
        } else {
            None
        }
    }

    /// Reads `text` as a script `depth` scripts deep, run with `CLAUDECODE`
    /// cleared when `cleared`.
    fn script(&mut self, text: &str, depth: u32, cleared: bool) {
        // the call's own text reads in linear time: only re-reads count
        if depth > 0 && self.spend(text.len()) {
            return;
        }
        if depth > MAX_DEPTH {
            self.by_words(text, cleared);
            return;
        }
        let Ok(script) = parse(text) else {
            self.by_words(text, cleared);
            return;
        };
        // what brace expansion left unexpanded can't be known, command
        // names included (`{git,push}`), and no reading of the words sees
        // through braces
        self.maybe_push |= script.unexpanded;
        for sub in &script.substitutions {
            self.script(sub, depth + 1, cleared);
        }
        for (i, cmd) in script.commands.iter().enumerate() {
            let clears = cmd
                .assigns
                .iter()
                .any(|(n, v)| n == "CLAUDECODE" && clears(v));
            if cmd.words.is_empty() {
                self.clears_claudecode |= clears;
                continue;
            }
            self.clearing_builtin(&cmd.words);
            self.note_written(cmd);
            let stdin = || stdin_of(&script, i);
            self.argv(&cmd.words, depth, cleared || clears, &stdin);
        }
    }

    /// Counts `n` toward `MAX_WORK`: true, and nothing more is read, once
    /// past it.
    const fn spend(&mut self, n: usize) -> bool {
        self.work = self.work.saturating_add(n);
        self.exhausted |= self.work > MAX_WORK;
        self.exhausted
    }

    /// Text that couldn't be read, judged by its words: one command of it
    /// — a line, continuations joined, up to a `;`, `&`, or `|` — with a
    /// push word after a git word, quoting ignored (so `g"it` is git), is a
    /// push; so for `repos push` and `--new-branch`.
    fn by_words(&mut self, text: &str, cleared: bool) {
        let text: String = text
            .replace("\\\n", "")
            .chars()
            .filter(|c| !"'\"\\".contains(*c))
            .collect();
        self.clears_claudecode |= text.contains("CLAUDECODE");
        for command in text.split(['\n', ';', '&', '|']) {
            let words: Vec<&str> = command
                .split(|c: char| c.is_whitespace() || "()`${}<>".contains(c))
                .filter(|w| !w.is_empty())
                .collect();
            let has = |word: &str| words.contains(&word);
            let names = |name: &str| words.iter().any(|w| basename(w) == name);
            // a push word after a git word, as git's subcommand follows it
            let pushes_after_git =
                words
                    .iter()
                    .position(|w| basename(w) == "git")
                    .is_some_and(|g| {
                        words[g..]
                            .iter()
                            .any(|w| matches!(*w, "push" | "send-pack"))
                    });
            if pushes_after_git || names("git-push") || names("git-send-pack") {
                self.maybe_push = true;
            }
            if names("repos") && has("push") {
                self.runs_repos_push = true;
                self.repos_without_claudecode |= cleared;
                self.new_branch |= has("--new-branch");
            }
        }
    }

    /// `unset CLAUDECODE`, `export -n CLAUDECODE`, `export CLAUDECODE=`,
    /// and the like, which clear it for the rest of the call.
    fn clearing_builtin(&mut self, words: &[String]) {
        let Some(name) = words.first() else { return };
        let args = &words[1..];
        let named = || args.iter().any(|w| w == "CLAUDECODE");
        let emptied = || {
            args.iter()
                .filter_map(|w| split_assignment(w))
                .any(|(n, v)| n == "CLAUDECODE" && clears(v))
        };
        let unexported = || args.iter().any(|w| w == "-n" || w.starts_with('+'));
        self.clears_claudecode |= match name.as_str() {
            "unset" => named(),
            "export" | "declare" | "typeset" | "local" | "readonly" => {
                emptied() || (named() && unexported())
            }
            _ => false,
        };
    }

    /// Notes the script text `cmd` writes to files.
    fn note_written(&mut self, cmd: &SimpleCommand) {
        let (name, args) = (basename(&cmd.words[0]), &cmd.words[1..]);
        let (text, mut files) = match name {
            "cat" if !cmd.stdin.is_empty() => (cmd.stdin.clone(), vec![]),
            "tee" if !cmd.stdin.is_empty() => {
                let files = args.iter().filter(|a| !a.starts_with('-')).cloned();
                (cmd.stdin.clone(), files.collect())
            }
            "echo" | "printf" => (vec![args.join(" ").replace("\\n", "\n")], vec![]),
            _ => return,
        };
        files.extend(cmd.writes.iter().cloned());
        for file in files {
            self.written
                .entry(script_path(&file).to_owned())
                .or_default()
                .extend(text.iter().cloned());
        }
    }

    /// Reads what the call wrote to `file`, when it runs it.
    fn run_written(&mut self, file: &str, depth: u32, cleared: bool) {
        if let Some(texts) = self.written.get(script_path(file)).cloned() {
            for text in texts {
                self.script(&text, depth + 1, cleared);
            }
        }
    }

    /// Reads one argv: past assignments and wrappers to what it runs.
    /// `stdin` gives the text on its stdin the script holds, read only for
    /// a shell.
    fn argv(&mut self, words: &[String], depth: u32, cleared: bool, stdin: Stdin<'_>) {
        if depth > 0 && self.spend(words.iter().map(|w| w.len() + 1).sum()) {
            return;
        }
        if depth > MAX_DEPTH {
            self.by_words(&words.join(" "), cleared);
            return;
        }
        let mut cleared = cleared;
        let mut i = 0;
        while let Some(word) = words.get(i) {
            let rest = &words[i + 1..];
            let base = basename(word);
            if base.contains(DYN) {
                self.git(rest, depth, cleared, true);
                self.shell(rest, &no_stdin, depth, cleared, true);
                return;
            }
            if let Some((name, value)) = split_assignment(word) {
                cleared |= name == "CLAUDECODE" && clears(value);
                i += 1;
                continue;
            }
            if self.written.contains_key(script_path(word)) {
                return self.run_written(word, depth, cleared);
            }
            let next = match base {
                "git" => return self.git(rest, depth, cleared, false),
                "git-push" | "git-send-pack" => {
                    self.push = true;
                    return;
                }
                "repos" => return self.repos(rest, cleared),
                "eval" => Next::Script(rest.join(" ")),
                "source" | "." => {
                    if let Some(file) = rest.first() {
                        self.run_written(file, depth, cleared);
                    }
                    return;
                }
                "gro" => {
                    if rest.first().is_some_and(|w| w == "gitops_run") {
                        for payload in &rest[1..] {
                            self.script(payload, depth + 1, cleared);
                        }
                    }
                    return;
                }
                s if SHELLS.contains(&s) => return self.shell(rest, stdin, depth, cleared, false),
                "env" => env(rest, &mut cleared),
                "sudo" => Next::At(past_options(rest, SUDO_VALUES)),
                "doas" => Next::At(past_options(rest, &["-u", "-C"])),
                "timeout" => {
                    Next::At(past_options(rest, &["-s", "-k", "--signal", "--kill-after"]) + 1)
                }
                "nice" => Next::At(past_options(rest, &["-n", "--adjustment"])),
                "ionice" => Next::At(past_options(rest, &["-c", "-n", "--class", "--classdata"])),
                "time" => Next::At(past_options(rest, &["-f", "-o", "--format", "--output"])),
                "exec" => Next::At(past_options(rest, &["-a"])),
                "stdbuf" => Next::At(past_options(
                    rest,
                    &["-i", "-o", "-e", "--input", "--output", "--error"],
                )),
                "chrt" => Next::At(
                    past_options(
                        rest,
                        &[
                            "-T",
                            "-P",
                            "-D",
                            "--sched-runtime",
                            "--sched-period",
                            "--sched-deadline",
                        ],
                    ) + 1,
                ),
                "taskset" => Next::At(past_options(rest, &[]) + 1),
                "nohup" | "setsid" | "builtin" | "unbuffer" | "chronic" | "busybox" => {
                    Next::At(past_options(rest, &[]))
                }
                "command" => command(rest),
                "flock" => flock(rest),
                "su" => su(rest),
                "watch" => watch(rest),
                "xargs" => Next::Argv(xargs(rest)),
                "find" => {
                    for exec in find_execs(rest) {
                        self.argv(&exec, depth + 1, cleared, &no_stdin);
                    }
                    return;
                }
                s if RESERVED.contains(&s) => Next::At(0),
                s if RESERVED_DATA.contains(&s) || TEXT_COMMANDS.contains(&s) => return,
                // an unknown command: look past it, as for a wrapper
                _ => match rest.iter().position(|w| starts_command(basename(w))) {
                    Some(k) => Next::At(k),
                    None => return,
                },
            };
            match next {
                Next::At(k) => i += 1 + k,
                Next::Argv(argv) => return self.argv(&argv, depth + 1, cleared, stdin),
                Next::Script(text) => return self.script(&text, depth + 1, cleared),
                Next::Stop => return,
            }
        }
    }

    /// Reads git's arguments: its global options, then the subcommand.
    /// `unknown` when the command name only may be git: then only a
    /// literal push counts.
    fn git(&mut self, args: &[String], depth: u32, cleared: bool, unknown: bool) {
        let mut i = 0;
        while let Some(a) = args.get(i) {
            match a.as_str() {
                "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--attr-source"
                | "--super-prefix" => i += 2,
                "-c" => {
                    self.push |= !unknown && args.get(i + 1).is_some_and(|v| alias_pushes(v));
                    i += 2;
                }
                "--config-env" => {
                    self.push |= !unknown && args.get(i + 1).is_some_and(|v| is_alias(v));
                    i += 2;
                }
                // print and exit, or name `help`/`version` as the command
                "-h" | "--help" | "-v" | "--version" | "--html-path" | "--man-path"
                | "--info-path" | "--exec-path" => return,
                s if s.starts_with("--list-cmds=") => return,
                s if s.starts_with("--config-env=") => {
                    self.push |= !unknown && is_alias(&s["--config-env=".len()..]);
                    i += 1;
                }
                s if s.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(i) else { return };
        let rest = &args[i + 1..];
        if matches!(sub.as_str(), "push" | "send-pack") {
            self.push = true;
            return;
        }
        if unknown {
            return;
        }
        match sub.as_str() {
            // a subcommand it can't know may be a push
            s if s.contains(DYN) => self.push = true,
            "config" => {
                self.push |= rest
                    .iter()
                    .position(|w| is_alias(w))
                    .is_some_and(|k| rest[k + 1..].iter().any(|w| pushes(w)));
            }
            "submodule" => {
                if let Some(k) = rest.iter().position(|w| w == "foreach") {
                    let cmd: Vec<&str> = rest[k + 1..]
                        .iter()
                        .map(String::as_str)
                        .skip_while(|w| matches!(*w, "--recursive" | "-q" | "--quiet"))
                        .collect();
                    self.script(&cmd.join(" "), depth + 1, cleared);
                }
            }
            "bisect" if rest.first().is_some_and(|w| w == "run") => {
                self.argv(&rest[1..], depth + 1, cleared, &no_stdin);
            }
            "rebase" => {
                for (k, w) in rest.iter().enumerate() {
                    let payload = match w.as_str() {
                        "-x" | "--exec" => rest.get(k + 1).map(String::as_str),
                        w => w
                            .strip_prefix("--exec=")
                            .or_else(|| w.strip_prefix("-x").filter(|p| !p.is_empty())),
                    };
                    if let Some(p) = payload {
                        self.script(p, depth + 1, cleared);
                    }
                }
            }
            _ => {}
        }
    }

    /// Reads `repos`'s arguments: only `push` (or a subcommand it can't
    /// know) is gated by `CLAUDECODE`.
    fn repos(&mut self, args: &[String], cleared: bool) {
        let mut i = 0;
        while let Some(a) = args.get(i) {
            match a.as_str() {
                "--registry" | "--root" => i += 2,
                s if s.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(i) else { return };
        if sub == "push" || sub.contains(DYN) {
            self.runs_repos_push = true;
            self.repos_without_claudecode |= cleared;
        }
        if sub == "push" {
            self.new_branch |= args[i + 1..].iter().any(|w| w == "--new-branch");
        }
    }

    /// Reads a shell's arguments: a `-c` script, or its stdin when it runs
    /// no file. `unknown` when the command name only may be a shell: then
    /// only `-c` counts.
    fn shell(
        &mut self,
        args: &[String],
        stdin: Stdin<'_>,
        depth: u32,
        cleared: bool,
        unknown: bool,
    ) {
        let mut i = 0;
        let mut command = false;
        let mut reads_stdin = false;
        while let Some(a) = args.get(i) {
            if a == "--" {
                i += 1;
                break;
            }
            if let Some(payload) = a.strip_prefix("--command=") {
                self.script(payload, depth + 1, cleared);
                return;
            }
            if a == "--command" {
                command = true;
                i += 1;
                break;
            }
            if a.starts_with("--") {
                i += if matches!(a.as_str(), "--rcfile" | "--init-file") {
                    2
                } else {
                    1
                };
                continue;
            }
            if (a.starts_with('-') || a.starts_with('+')) && a.len() > 1 {
                let flags = &a[1..];
                // `-n` reads commands without running any
                if a.starts_with('-') && flags.contains('n') {
                    return;
                }
                command |= flags.contains('c');
                reads_stdin |= flags.contains('s');
                i += if flags.contains('o') || flags.contains('O') {
                    2
                } else {
                    1
                };
                continue;
            }
            break;
        }
        if command {
            if let Some(payload) = args.get(i) {
                self.script(payload, depth + 1, cleared);
            }
            return;
        }
        if unknown {
            return;
        }
        match args.get(i) {
            Some(file) if !reads_stdin && file != "-" => self.run_written(file, depth, cleared),
            _ => {
                let piped = stdin();
                // what the unread commands pipe in can't be known
                self.maybe_push |= piped.cut;
                for text in piped.texts {
                    self.script(&text, depth + 1, cleared);
                }
            }
        }
    }
}

/// A file's path as the call names it, a leading `./` dropped.
fn script_path(file: &str) -> &str {
    file.strip_prefix("./").unwrap_or(file)
}

/// Whether a `CLAUDECODE` value hides the agent: empty, or unknown.
fn clears(value: &str) -> bool {
    value.is_empty() || value.contains(DYN)
}

/// Whether a word may say push: as a git alias's value.
fn pushes(word: &str) -> bool {
    word.contains("push") || word.contains("send-pack") || word.contains(DYN)
}

/// A config key (or `key=value`) in git's `alias.` section.
fn is_alias(word: &str) -> bool {
    word.get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("alias."))
}

/// A `-c` value defining an alias that may push.
fn alias_pushes(setting: &str) -> bool {
    is_alias(setting) && setting.split_once('=').is_none_or(|(_, v)| pushes(v))
}

/// Whether a command name starts a command worth reading, looked for past
/// an unknown command name.
fn starts_command(base: &str) -> bool {
    matches!(
        base,
        "git"
            | "repos"
            | "eval"
            | "env"
            | "sudo"
            | "doas"
            | "timeout"
            | "nice"
            | "nohup"
            | "exec"
            | "command"
            | "setsid"
            | "stdbuf"
            | "flock"
            | "xargs"
            | "su"
    ) || SHELLS.contains(&base)
}

/// The index of the first word past `args`' options; `values` take the
/// next word. `--` ends them.
fn past_options(args: &[String], values: &[&str]) -> usize {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "--" {
            return i + 1;
        }
        if !a.starts_with('-') || a == "-" {
            return i;
        }
        i += if values.contains(&a.as_str()) { 2 } else { 1 };
    }
    // a value-taking option last: its value is missing
    i.min(args.len())
}

/// `env`'s options and assignments, noting when they clear `CLAUDECODE`.
fn env(args: &[String], cleared: &mut bool) -> Next {
    let hides = |name: Option<&String>| name.is_none_or(|n| n == "CLAUDECODE" || n.contains(DYN));
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--" => return Next::At(i + 1),
            "-" | "-i" | "--ignore-environment" => {
                *cleared = true;
                i += 1;
            }
            "-u" | "--unset" => {
                *cleared |= hides(args.get(i + 1));
                i += 2;
            }
            "-C" | "--chdir" => i += 2,
            "-S" | "--split-string" => {
                let rest = args.get(i + 2..).unwrap_or_default();
                return split_string(args.get(i + 1), rest);
            }
            s if s.starts_with("--split-string=") => {
                return split_string(
                    Some(&s["--split-string=".len()..].to_owned()),
                    &args[i + 1..],
                );
            }
            s if s.starts_with("-S") => {
                return split_string(Some(&s[2..].to_owned()), &args[i + 1..]);
            }
            s if s.starts_with("--unset=") => {
                *cleared |= hides(Some(&s["--unset=".len()..].to_owned()));
                i += 1;
            }
            s if s.starts_with("-u") => {
                *cleared |= hides(Some(&s[2..].to_owned()));
                i += 1;
            }
            s if s.starts_with('-') => {
                // a cluster with `i` (`-iv`) clears the environment
                *cleared |= !s.starts_with("--") && s.contains('i');
                i += 1;
            }
            s => match split_assignment(s) {
                Some((name, value)) => {
                    *cleared |= name == "CLAUDECODE" && clears(value);
                    i += 1;
                }
                None => return Next::At(i),
            },
        }
    }
    Next::At(i)
}

/// `env -S`'s string split into words, before the rest.
fn split_string(string: Option<&String>, rest: &[String]) -> Next {
    let mut argv: Vec<String> = string
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    argv.extend_from_slice(rest);
    Next::Argv(argv)
}

/// `command`, which runs its command unless it only describes it (`-v`,
/// `-V`).
fn command(args: &[String]) -> Next {
    let at = past_options(args, &[]);
    let describes = args[..at]
        .iter()
        .any(|a| !a.starts_with("--") && (a.contains('v') || a.contains('V')));
    if describes { Next::Stop } else { Next::At(at) }
}

/// `flock`: its options, a lock file, then a command, or a script under
/// `-c`.
fn flock(args: &[String]) -> Next {
    let mut i = 0;
    let mut file = false;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-c" | "--command" => {
                return args
                    .get(i + 1)
                    .map_or(Next::Stop, |s| Next::Script(s.clone()));
            }
            s if s.starts_with("--command=") => {
                return Next::Script(s["--command=".len()..].to_owned());
            }
            "--" => return Next::At(i + 1),
            "-w" | "--timeout" | "-E" | "--conflict-exit-code" => i += 2,
            s if s.starts_with('-') && s.len() > 1 => i += 1,
            _ if !file => {
                file = true;
                i += 1;
            }
            _ => break,
        }
    }
    Next::At(i)
}

/// `su`, which runs a command only under `-c`.
fn su(args: &[String]) -> Next {
    for (i, a) in args.iter().enumerate() {
        match a.as_str() {
            "-c" | "--command" | "--session-command" => {
                return args
                    .get(i + 1)
                    .map_or(Next::Stop, |s| Next::Script(s.clone()));
            }
            s => {
                if let Some(s) = s
                    .strip_prefix("--command=")
                    .or_else(|| s.strip_prefix("--session-command="))
                {
                    return Next::Script(s.to_owned());
                }
            }
        }
    }
    Next::Stop
}

/// `watch`, which runs its words through `sh -c`, or as an argv under
/// `-x`.
fn watch(args: &[String]) -> Next {
    let at = past_options(args, &["-n", "--interval", "-q", "--equexit"]);
    let exec = args[..at].iter().any(|a| a == "-x" || a == "--exec");
    let cmd = args[at..].to_vec();
    if exec {
        Next::Argv(cmd)
    } else {
        Next::Script(cmd.join(" "))
    }
}

/// The argv `xargs` runs: its command, the replace string made `DYN`, and
/// the arguments it reads from stdin, which can't be known.
fn xargs(args: &[String]) -> Vec<String> {
    let mut i = 0;
    let mut replace: Option<String> = None;
    while let Some(a) = args.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        match a.as_str() {
            "-I" | "--replace" => {
                replace = args.get(i + 1).cloned();
                i += 2;
                continue;
            }
            s if s.starts_with("--replace=") => replace = Some(s["--replace=".len()..].to_owned()),
            s if s.starts_with("-I") => replace = Some(s[2..].to_owned()),
            s if s.starts_with("-i") => {
                replace = Some(if s.len() > 2 {
                    s[2..].to_owned()
                } else {
                    "{}".to_owned()
                });
            }
            _ => {}
        }
        i += if XARGS_VALUES.contains(&a.as_str()) {
            2
        } else {
            1
        };
    }
    let mut argv: Vec<String> = args
        .get(i..)
        .unwrap_or_default()
        .iter()
        .map(|w| match &replace {
            Some(r) if !r.is_empty() => w.replace(r.as_str(), &DYN.to_string()),
            _ => w.clone(),
        })
        .collect();
    argv.push(DYN.to_string());
    argv
}

/// The argvs `find` runs under `-exec`, `-execdir`, `-ok`, and `-okdir`,
/// `{}` made `DYN`.
fn find_execs(args: &[String]) -> Vec<Vec<String>> {
    let mut execs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let exec: Vec<String> = args[i + 1..]
                .iter()
                .take_while(|w| *w != ";" && *w != "+")
                .map(|w| w.replace("{}", &DYN.to_string()))
                .collect();
            i += exec.len() + 1;
            execs.push(exec);
        }
        i += 1;
    }
    execs
}

/// The text on a command's stdin that the script holds: its own
/// here-documents and here-strings, and, when it reads a pipe, the words
/// and stdin of each command before it in the pipeline (`echo 'git push' |
/// bash`), up to `MAX_PIPELINE` back, `\n` in them read as a newline —
/// past it, `cut`.
fn stdin_of(script: &Script, i: usize) -> Piped {
    let commands = &script.commands;
    let mut stdin = commands[i].stdin.clone();
    let mut at = i;
    while commands[at].piped && at > 0 && i - at < MAX_PIPELINE {
        at -= 1;
        let SimpleCommand {
            words, stdin: s, ..
        } = &commands[at];
        if words.len() > 1 {
            stdin.push(words[1..].join(" ").replace("\\n", "\n"));
        }
        stdin.extend(s.iter().cloned());
    }
    Piped {
        texts: stdin,
        cut: commands[at].piped && at > 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn denied(command: &str, denial: Denial) {
        assert_eq!(check_command(command), Some(denial), "{command}");
    }

    #[track_caller]
    fn passes(command: &str) {
        assert_eq!(check_command(command), None, "{command}");
    }

    /// The deny rule `Bash(git push:*)` catches these; the hook denies them
    /// too.
    #[test]
    fn what_the_deny_rule_catches() {
        for c in [
            "git push",
            "cd x && git push",
            "true; git push",
            "(git push | cat)",
            "$(git push)",
            "env A=1 git push",
            "FOO=1 git push",
            "timeout 5 git push",
            "command git push",
            "exec git push",
            "\\git push",
            "git   push",
            "g=git; $g push",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    /// The deny rule misses these; the hook is what denies them.
    #[test]
    fn what_the_deny_rule_misses() {
        for c in [
            "git -C dir push",
            "git -c k=v push",
            "git --git-dir=.git push",
            "/usr/bin/git push",
            "\"git\" \"push\"",
            "git pu''sh",
            "bash -c 'git push'",
            "sh -c \"git push\"",
            "eval \"git push\"",
            "bash <<'EOF'\ngit push\nEOF",
            "/usr/lib/git-core/git-push",
            // brace expansion, letter sequences and words it empties
            "git pus{h..h}",
            "gi{t..t} push",
            "{g..g}it push",
            "git {p..p}ush origin main",
            "sudo gi{t..t} pus{h..h}",
            "git {p..z..10}ush",
            // not a sequence to bash: `-C` takes `{a..b}`
            "git -C {a..''b} push",
            "{git,} push",
            "git {,} push",
            "git -C {,} . push",
            "x=; git $x push",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    #[test]
    fn git_and_its_global_options() {
        for c in [
            "g''it push",
            "\"g\"it push origin main",
            "~/bin/git push",
            "git --git-dir .git --work-tree w push",
            "git --work-tree=w --namespace=n --namespace n push",
            "git --exec-path=/x --config-env=a.b=V --config-env a.b=V push",
            "git -P --no-pager -p --paginate --bare push",
            "git --no-replace-objects --literal-pathspecs --no-lazy-fetch --no-optional-locks \
             --no-advice --glob-pathspecs --attr-source HEAD push",
            "git -C a -C b -c x=1 -c y=2 push --force",
            "git send-pack origin main",
            "git-send-pack origin main",
            "/usr/lib/git-core/git-send-pack",
            "$(git --exec-path)/git-push",
            "git $'\\x70ush'",
            "git pu{sh,ll}",
            "git \\\npush",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    #[test]
    fn wrappers() {
        for c in [
            "env -u X git push",
            "env -i PATH=/bin git push",
            "env -C /tmp -- git push",
            "env -S 'git push'",
            "sudo git push",
            "sudo -u me -E git push",
            "doas -u me git push",
            "timeout -s KILL 5m git push",
            "nice git push",
            "nice -n 5 git push",
            "ionice -c 3 git push",
            "nohup git push &",
            "time git push",
            "time -p git push",
            "command -p git push",
            "exec -a x git push",
            "builtin exec git push",
            "setsid -w git push",
            "stdbuf -oL -e L git push",
            "flock /tmp/l git push",
            "flock -w 5 /tmp/l git push",
            "flock /tmp/l -c 'git push'",
            "chrt -f 10 git push",
            "taskset -c 0 git push",
            "busybox sh -c 'git push'",
            "su -c 'git push' me",
            "watch -n 5 git push",
            "watch -x git push",
            "xargs git push",
            "echo push | xargs git",
            "echo push | xargs -n1 git -C dir",
            "xargs -I{} git {} origin",
            "find . -name .git -execdir git push \\;",
            "find . -exec git push {} +",
            "if git push; then :; fi",
            "while true; do git push; done",
            "! git push",
            "{ git push; }",
            "f() { git push; }",
            "case x in y) git push;; esac",
            "unknown-wrapper --flag git push",
            "retry 3 git -C x push",
            "ssh host git push",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    #[test]
    fn scripts_in_scripts() {
        for c in [
            "zsh -c 'git push'",
            "dash -c 'git push'",
            "fish -c 'git push'",
            "fish --command='git push'",
            "ksh -c 'git push'",
            "bash -lc 'git push'",
            "bash -e -o pipefail -c 'git push'",
            "bash --norc -c 'git push'",
            "/bin/bash -c 'cd x && git push'",
            "sh -c 'git \"$1\"' _ push",
            "sh -c \"sh -c 'git push'\"",
            "eval git pu''sh",
            "eval 'g=git; $g push'",
            "bash <<< 'git push'",
            "bash -s <<EOF\ngit push\nEOF",
            "sh - <<EOF\ncd x\ngit push\nEOF",
            "cat <<'EOF' | bash\ngit push\nEOF",
            "cat > /tmp/p.sh <<'EOF'\nset -e\ngit -C x push origin main 2>&1\nEOF\nbash /tmp/p.sh",
            "S=/tmp/s; cat > $S/p.sh <<EOF\ngit push\nEOF\nchmod +x $S/p.sh && $S/p.sh",
            "tee p.sh >/dev/null <<'EOF'\ngit push\nEOF\nsh ./p.sh",
            "echo 'git push' > p.sh; . p.sh",
            "printf 'cd x\\ngit push\\n' >> p.sh && source ./p.sh",
            "echo 'git push' | bash",
            "printf 'cd x\\ngit push\\n' | sh",
            "echo 'git push' |\nbash",
            "echo 'git push' | # then\nbash",
            "echo 'git push' | (bash)",
            "echo 'git push' | { bash; }",
            "echo 'git push' | (cat) | bash",
            "echo 'git push' | { true; bash; }",
            "echo 'git push' | (true; (bash))",
            "cat <<$x\n$(git push)\n$x",
            "x=Q; cat <<\"$x\"\n$x\ngit push\nQ",
            "cat <<$(x)\nhi\n$(x)\ngit push",
            // a backquoted delimiter is taken as written, backquotes and all
            "cat <<`a b`\nhi\n`a b`\ngit push",
            "echo `git push`",
            "x=$(git push)",
            "echo \"$(git push)\"",
            "diff <(git push) y",
            "cat <<EOF\n$(git push)\nEOF",
            "echo ${x:-$(git push)}",
            // a `}` in the substitution closes nothing: the push runs
            "g=git; echo ${x:-$(cat <<'EOF'\n}\nEOF\n$g push\n)}",
            "echo \"${x:-$(echo }; git push)}\"",
            "x=$(echo ${y:-)}; git push)",
            "$SHELL -c 'git push'",
            "gro gitops_run 'git push'",
            "gro gitops_run --concurrency 2 \"git push origin main\"",
            "git submodule foreach 'git push'",
            "git submodule foreach --recursive git push",
            "git bisect run git push",
            "git rebase -x 'git push' main",
            "git rebase --exec='git push' main",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    #[test]
    fn what_git_may_run_as_a_push() {
        for c in [
            "$g push",
            "\"$GIT\" -C x push",
            "$(which git) push",
            "`which git` push",
            "git $cmd",
            "git \"$@\"",
            "git -c alias.p=push p",
            "git -c alias.p='!git push' p",
            "git -c Alias.P=push p",
            "git --config-env=alias.p=CMD p",
            "git config alias.p push",
            "git config --global alias.p '!git push origin'",
            "git config set alias.sp send-pack",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    #[test]
    fn unreadable_text_that_names_a_push() {
        for c in [
            "git push \"",
            "git push 'origin",
            "echo $(git push",
            "g\"it pu\"sh '",
            "bash -c 'git push",
        ] {
            denied(c, Denial::Unreadable);
        }
        // nesting past the limit is judged by its words
        let depth = MAX_DEPTH as usize;
        let nested = |n| format!("{}git push{}", "echo $(".repeat(n), ")".repeat(n));
        denied(&nested(depth), Denial::GitPush);
        denied(&nested(depth + 1), Denial::Unreadable);
        passes(&nested(depth + 1).replace("push", "status"));
        // unreadable, with no push in it
        passes("echo \"it's");
        passes("git commit -m 'wip");
        // git and push in one command of it, continuations joined; in two,
        // no push
        denied("cd x && git \\\npush 'y", Denial::Unreadable);
        denied("git-push 'x", Denial::Unreadable);
        passes("git log --oneline; echo 'push");
        passes("git status\necho \"don't push");
        passes("git log | grep 'push");
        passes("echo 'push the git repo");
        passes("repos status; echo '--new-branch push");
        denied("repos push --new-branch '", Denial::NewBranch);
    }

    /// Claude Code's commit and PR idiom: a quoted here-document in a
    /// substitution, whose body may hold any text.
    #[test]
    fn here_documents_in_substitutions() {
        for c in [
            "git commit -m \"$(cat <<'EOF'\nfix: don't push tags\nEOF\n)\"",
            "git commit -m \"$(cat <<'EOF'\nfix: a lone ` and a ) in git push\nEOF\n)\"",
            "gh pr create --title t --body \"$(cat <<'EOF'\n## Summary\n- don't git push\nEOF\n)\"",
            "git tag -a v1 -m \"$(cat <<\"EOF\"\nwon't push tags\nEOF\n)\"",
            "x=$(cat <<'EOF'\ndon't push\nEOF\n); echo \"$x\"",
            "x=$(cat <<-'EOF'\n\tdon't push\n\tEOF\n)",
            "x=$(cat <<EOF | tr a b\nit's git push\nEOF\n)",
            "echo $(cat <<< \"don't\") $((1 << 2))",
            "echo ${x:-$(echo })}",
            "echo \"${x:-$(cat <<'EOF'\ndon't } push\nEOF\n)}\"",
            "x=$(# don't\ngit status)",
        ] {
            passes(c);
        }
        for c in [
            "x=$(bash <<'EOF'\ngit push\nEOF\n)",
            "x=$(cat <<'EOF'\ndon't\nEOF\ngit push)",
            "echo \"$(sh <<-X\n\tgit push\n\tX\n)\"",
        ] {
            denied(c, Denial::GitPush);
        }
    }

    /// A shell's stdin is read `MAX_PIPELINE` commands back; past that,
    /// what the rest pipe in can't be known.
    #[test]
    fn a_pipeline_past_its_bound_is_unreadable() {
        let cats = |n: usize| "| cat ".repeat(n);
        denied(
            &format!("echo 'git push' {}| bash", cats(MAX_PIPELINE - 1)),
            Denial::GitPush,
        );
        denied(
            &format!("echo 'git push' {}| bash", cats(MAX_PIPELINE)),
            Denial::Unreadable,
        );
        denied(
            &format!("echo hi {}| sh -s", cats(MAX_PIPELINE)),
            Denial::Unreadable,
        );
        // a newline or group after a pipe doesn't end it
        denied(
            &format!("echo 'git push' {}|\n(bash)", cats(MAX_PIPELINE)),
            Denial::Unreadable,
        );
        denied(
            &format!(
                "echo 'git push' {}| {{ true; bash; }}",
                cats(MAX_PIPELINE - 2)
            ),
            Denial::GitPush,
        );
        // what reads no stdin as a script passes, however long
        passes(&format!("echo 'git push' {}", cats(MAX_PIPELINE * 2)));
        passes(&format!("echo hi {}| bash -c 'wc -l'", cats(MAX_PIPELINE)));
        passes(&format!("echo hi {}| bash run.sh", cats(MAX_PIPELINE)));
    }

    /// Every wrapper and command read, with each of its options last:
    /// a value it takes is missing.
    const TRAILING: &[(&str, &[&str])] = &[
        (
            "env",
            &[
                "-u",
                "--unset",
                "-C",
                "--chdir",
                "-S",
                "--split-string",
                "-i",
                "-",
            ],
        ),
        ("sudo", SUDO_VALUES),
        ("doas", &["-u", "-C"]),
        ("timeout", &["-s", "-k", "--signal", "--kill-after", "5"]),
        ("nice", &["-n", "--adjustment"]),
        ("ionice", &["-c", "-n", "--class", "--classdata"]),
        ("time", &["-f", "-o", "--format", "--output"]),
        ("exec", &["-a"]),
        (
            "stdbuf",
            &["-i", "-o", "-e", "--input", "--output", "--error"],
        ),
        (
            "chrt",
            &[
                "-T",
                "-P",
                "-D",
                "--sched-runtime",
                "--sched-period",
                "--sched-deadline",
                "5",
            ],
        ),
        ("taskset", &["-p", "3"]),
        (
            "flock",
            &[
                "-w",
                "--timeout",
                "-E",
                "--conflict-exit-code",
                "-c",
                "--command",
                "f",
            ],
        ),
        ("su", &["-c", "--command", "--session-command", "-"]),
        (
            "watch",
            &["-n", "--interval", "-q", "--equexit", "-x", "--exec"],
        ),
        ("xargs", XARGS_VALUES),
        ("xargs", &["-I", "--replace", "-i", "-0", "--"]),
        ("find", &[".", "-exec", "-execdir", "-ok", "-okdir"]),
        (
            "git",
            &[
                "-C",
                "-c",
                "--git-dir",
                "--work-tree",
                "--namespace",
                "--attr-source",
                "--super-prefix",
                "--config-env",
                "config",
                "submodule",
                "bisect",
                "rebase",
                "-x",
                "--exec",
            ],
        ),
        (
            "bash",
            &[
                "-c",
                "-o",
                "-O",
                "--rcfile",
                "--init-file",
                "--command",
                "-s",
                "-",
            ],
        ),
        ("repos", &["push", "--registry"]),
        ("command", &["-v", "-p"]),
        ("gro", &["gitops_run"]),
        ("nohup", &["--"]),
        ("setsid", &["-w"]),
        ("eval", &[""]),
        ("source", &[""]),
        ("unset", &["-v"]),
        ("export", &["-n"]),
    ];

    /// No input panics: the release build aborts on one, and an abort is
    /// an exit Claude Code doesn't block on — the hook would fail open.
    #[test]
    fn no_input_panics() {
        for (name, options) in TRAILING {
            for option in *options {
                for command in [
                    format!("git push; {name} {option}"),
                    format!("{name} {option}"),
                    format!("git push; {name} {{,}} {option}"),
                    format!("echo 'git push' | {name} {option}"),
                    format!("git push; sudo {name} x {option}"),
                ] {
                    let denial = check_command(&command);
                    if command.starts_with("git push") {
                        assert_eq!(denial, Some(Denial::GitPush), "{command}");
                    }
                }
            }
        }
        // every prefix of commands that use much of the grammar
        let seeds = [
            "cd x && env -S 'git -C y push' | xargs -I{} sh -c \"echo {}\" && timeout -s 9 5 git pu{sh,ll}",
            "cat <<'EOF' | bash\ngit push $(echo `x`)\nEOF\nx=$(( 1 + ${y%%q} )); ((z++))",
            "echo \"${a:-$(b <<-X\n\tq\n\tX\n)}\" <(c) >(d) $'\\x70' $\"e\" 2>&1 >>f &>g <<<h",
            "( { git -c alias.p=push p; } ) || watch -n 1 -x git push; find . -exec git push \\;",
            "for i in {1..3} {a..c}; do sudo -u me nice -n 5 flock f -c 'git push'; done # x",
            "f() { git -C \"$1\" push; }; export CLAUDECODE=; repos push --new-branch",
        ];
        for seed in seeds {
            for (at, _) in seed.char_indices() {
                check_command(&seed[..at]);
                check_command(&seed[at..]);
            }
        }
        // random commands of the grammar's pieces
        let pieces: Vec<&str> = TRAILING
            .iter()
            .flat_map(|(name, options)| std::iter::once(*name).chain(options.iter().copied()))
            .chain([
                "git",
                "push",
                "sh",
                "'",
                "\"",
                "\\",
                "$(",
                ")",
                "(",
                "{",
                "}",
                "{a,b}",
                "{,}",
                "{a..c}",
                "{z..a..2}",
                ",",
                "..",
                "|",
                "||",
                "&&",
                ";",
                "&",
                "<<",
                "<<-",
                "<<<",
                "EOF",
                "$x",
                "${",
                "${x:-",
                "`",
                "#",
                "=",
                "x=",
                "CLAUDECODE=",
                "$'",
                "$\"",
                "\n",
                ">",
                "2>&1",
                "--",
                "-",
                "((",
                "$((",
                "$[",
                "]",
                "{a..{b,c}}",
                "\\,",
                "--new-branch",
            ])
            .collect();
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % n as u64).unwrap_or(0)
        };
        for _ in 0..5_000 {
            let len = 1 + next(12);
            let mut command = String::new();
            for _ in 0..len {
                command.push_str(pieces[next(pieces.len())]);
                command.push_str([" ", "", " ", "\n"][next(4)]);
            }
            check_command(&command);
        }
    }

    #[test]
    fn a_shell_that_only_checks_syntax_runs_nothing() {
        passes("bash -n s.sh");
        passes("bash -n <<'EOF'\ngit push\nEOF");
        passes("sh -nx -c 'git push'");
        denied("bash -x -c 'git push'", Denial::GitPush);
    }

    /// Every input reads in bounded time: past `MAX_DEPTH` scripts and
    /// argvs, and the reader's `MAX_NESTING`, text is judged by its words.
    #[test]
    fn pathological_input_is_bounded() {
        let inputs = [
            "xargs ".repeat(20_000) + "git status",
            "find . -exec ".repeat(20_000) + "git status",
            "env -S ".repeat(20_000) + "git status",
            "watch -x ".repeat(20_000) + "git status",
            "eval ".repeat(50_000) + "git status",
            "${a:-".repeat(20_000) + &"}".repeat(20_000),
            "echo \"$(".repeat(5_000) + &")\"".repeat(5_000),
            "echo ".to_owned() + &"{".repeat(50_000),
            "$(".repeat(100_000) + "git status" + &")".repeat(100_000),
            "cat <<EOF | bash\n".to_owned() + &"echo $(echo $(echo x))\n".repeat(20_000) + "EOF",
        ];
        for input in &inputs {
            let start = std::time::Instant::now();
            assert_eq!(check_command(input), None, "{}", &input[..40]);
            let took = start.elapsed();
            // generous for an unoptimized build on a slow runner
            assert!(took.as_secs() < 5, "{took:?}: {}", &input[..40]);
        }
        // a pipeline into a shell past `MAX_PIPELINE`: bounded, and denied
        // as unreadable
        let start = std::time::Instant::now();
        denied(
            &("echo git status | ".repeat(20_000) + "bash"),
            Denial::Unreadable,
        );
        assert!(start.elapsed().as_secs() < 5);
        // brace expansion past a budget: bounded, and denied as unreadable,
        // since what the words left unexpanded run can't be known — a
        // command that size is never an ordinary one
        let words = "{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}{o,p} ".repeat(200_000);
        let unreadable = Some(Denial::Unreadable);
        for (input, denial) in [
            (format!("echo {words}"), unreadable),
            (format!("x {words}"), unreadable),
            (format!("x {words}; {{git,push}}"), unreadable),
            (format!("x {words}; sudo {{git,push}}"), unreadable),
            (format!("x {words}; env {{git,push}}"), unreadable),
            (format!("x {words}; timeout 5 {{git,push}}"), unreadable),
            (format!("x {words}; git pu{{sh,ll}}"), Some(Denial::GitPush)),
            (format!("x {words}; {{git,x}} push"), Some(Denial::GitPush)),
            // one word past the per-word budget: bash runs `git push origin
            // main -v …`
            (
                format!("{{git,push,origin,main{}}}", ",-v".repeat(22_000)),
                unreadable,
            ),
            (
                "echo ".to_owned() + &"{a,".repeat(20_000) + &"}".repeat(20_000),
                unreadable,
            ),
            // under both char budgets, but past the word cap: bash runs `git
            // -c x.y=z … push`
            (
                format!("git {{{}push}}", "-c,x.y=z,".repeat(300)),
                unreadable,
            ),
            (
                format!("{{git,{}push}}", "-c,x.y=z,".repeat(300)),
                unreadable,
            ),
        ] {
            let start = std::time::Instant::now();
            assert_eq!(
                check_command(&input),
                denial,
                "{}",
                &input[input.len() - 20..]
            );
            let took = start.elapsed();
            assert!(took.as_secs() < 5, "{took:?}: {}", &input[..40]);
        }
        // argvs wrappers hand on count toward `MAX_DEPTH`
        let depth = MAX_DEPTH as usize;
        denied(&("xargs ".repeat(depth) + "git push"), Denial::GitPush);
        denied(
            &("xargs ".repeat(depth + 1) + "git push"),
            Denial::Unreadable,
        );
        denied(
            &("env -S ".repeat(depth + 1) + "git push"),
            Denial::Unreadable,
        );
        // the call's own text, and its argvs, are read whole, however long
        let long = format!("echo {}; ", "a".repeat(MAX_WORK));
        assert!(long.len() > MAX_WORK);
        passes(&long);
        for tail in ["x=push; git $x", "g=git; $g push", "git -c alias.p=push p"] {
            denied(&(long.clone() + tail), Denial::GitPush);
        }
        let doc = "run git push, then repos push --new-branch\n".repeat(MAX_WORK / 40 + 1);
        passes(&format!("cat > doc.md <<'EOF'\n{doc}EOF"));
        // past `MAX_WORK` inside it, the whole call is judged by its words
        denied(&format!("bash -c '{long}git push'"), Denial::Unreadable);
        denied(
            &format!("bash -c '{long}' && repos push --new-branch"),
            Denial::NewBranch,
        );
        passes(&format!("bash -c '{long}'"));
    }

    /// Arithmetic has no here-document or comment: a `<<` in it never
    /// hides the lines after it, and bash runs `((cmd) )` as a subshell.
    #[test]
    fn arithmetic() {
        for c in [
            "echo $(( a <<b\n ))\ngit push\nb\n))",
            "(( x = 1 <<b ))\ngit push\nb",
            "for (( i = 1 <<b ; i < 1; i++ )); do :; done\ngit push\nb",
            "echo $[1<<2]\ngit push",
            "echo \"$[1<<b]\"\ngit push\nb",
            "x=$(echo $(( 1 <<b ))\ngit push\nb\n)",
            "x=$( (( 1 <<b ))\ngit push\nb\n)",
            "echo $(( $(git push) + 1 ))",
            "((git push) )",
            "echo $((git push) )",
            "(( $(git push) ))",
            "echo $[ $(git push) ]",
            "echo $(( 1 #))\ngit push",
            // read as a here-document, these would run to the end of the text
            "x=$( (( 1 <<b ))\n); git push",
            "x=$(echo $[1<<b]\n); git push",
            "x=$(echo $(( 1 <<b ))\n); git push",
        ] {
            denied(c, Denial::GitPush);
        }
        for c in [
            "echo $((1 << 2)) $[3 << 1]",
            "(( i++ )); for ((i = 0; i < 3; i++)); do echo $i; done",
            "x=$(( 2 # not a comment\n + 1 ))",
            "echo $(cat <<'EOF'\nit's $((1<<2))\nEOF\n)",
        ] {
            passes(c);
        }
    }

    #[test]
    fn what_passes() {
        for c in [
            "git status",
            "git log --grep push",
            "git log -- push",
            "echo \"git push\"",
            "echo git push",
            "printf '%s\\n' 'git push'",
            "grep -r 'git push' docs/",
            "rg 'git push' src",
            "git stash push",
            "git stash push -m wip -- src",
            // a `{` past a command's start is an argument: `bash` reads no pipe
            "echo 'git push' | grep { ; bash",
            "git remote set-url --push origin git@github.com:me/x",
            "git config push.default simple",
            "git config --get alias.push",
            "git config alias.st status",
            "git -c alias.st=status st",
            "git -C push status",
            "git show push",
            "git help push",
            "git --help push",
            "git --version",
            "man git-push",
            "which git",
            "command -v git push",
            "repos push",
            "repos push gro",
            "repos --registry r push gro",
            "repos status --fetch",
            "cargo test push",
            "ls push",
            "git commit -m \"use git push\"",
            "git commit -m 'git push is denied'",
            "git status # then git push",
            "cat <<'EOF'\ngit push\nEOF",
            "cat <<\"$x\"\n$(git push)\n$x",
            "echo 'git push' | (true); bash",
            "cat > notes.md <<'EOF'\nthen git push\nEOF",
            "cat > p.sh <<'EOF'\ngit push\nEOF\nbash other.sh",
            "echo 'git push' > p.sh; cat p.sh",
            "git commit -F- <<EOF\nrun git push\nEOF",
            "echo 'git push' > notes.txt",
            "echo 'git push' | cat",
            "gh pr create --body \"then git push\"",
            "git fetch && git status && git log --oneline -3",
            "bash script.sh push",
            "bash -c 'git status'",
            "sh -c \"echo 'git push'\"",
            "eval echo git push",
            "for x in git push; do echo $x; done",
            "$EDITOR push.md",
            "\"$CARGO\" test push",
            "xargs git add",
            "gro gitops_run 'git status'",
            "find . -exec git status \\;",
            "git submodule foreach git status",
            "git rebase -i main",
            "unset CLAUDECODE; cargo test",
            "CLAUDECODE= cargo test",
            "CLAUDECODE=1 repos status",
            "env CLAUDECODE=1 repos push",
            "echo $CLAUDECODE; repos status",
            "echo {a..z}",
            "for c in {a..e}; do echo $c; done",
            "echo {1..1000}",
            "touch f{1..500}.txt",
            "for i in {1..1000}; do git status; done",
            "",
            "   ",
        ] {
            passes(c);
        }
    }

    #[test]
    fn a_new_remote_branch_is_the_users() {
        for c in [
            "repos push --new-branch",
            "repos push gro --new-branch",
            "repos push --new-branch gro .",
            "repos --registry r --root d push --new-branch",
            "~/.cargo/bin/repos push --new-branch",
            "cd x && repos push --new-branch --json",
            "bash -c 'repos push --new-branch'",
            "timeout 60 repos push --new-branch",
            "repos push '--new-branch'",
        ] {
            denied(c, Denial::NewBranch);
        }
        passes("repos status --new-branch");
    }

    #[test]
    fn repos_keeps_claudecode() {
        for c in [
            "CLAUDECODE= repos push",
            "CLAUDECODE='' repos push gro",
            "CLAUDECODE=$X repos push",
            "CLAUDECODE= repos push $x",
            "CLAUDECODE= repos $cmd",
            "env CLAUDECODE= repos push",
            "env -u CLAUDECODE repos push",
            "env -uCLAUDECODE repos push",
            "env --unset=CLAUDECODE repos push",
            "env -i repos push",
            "env - repos push",
            "sudo CLAUDECODE= repos push",
            "unset CLAUDECODE; repos push",
            "unset CLAUDECODE && cd x && repos push",
            "export CLAUDECODE=; repos push",
            "export -n CLAUDECODE; repos push",
            "CLAUDECODE=; repos --registry r push",
            "env -u CLAUDECODE bash -c 'repos push'",
            "CLAUDECODE= sh -c 'cd x && repos push'",
            "bash -c 'unset CLAUDECODE; repos push'",
        ] {
            denied(c, Denial::Claudecode);
        }
        // a push first: the push is what it says
        denied("CLAUDECODE= repos push --new-branch", Denial::NewBranch);
        denied("unset CLAUDECODE; git push", Denial::GitPush);
        // only `push` is gated by it
        for c in [
            "env -u CLAUDECODE repos status",
            "CLAUDECODE= repos status",
            "CLAUDECODE= repos sync gro",
            "unset CLAUDECODE; repos status --json",
            "export -n CLAUDECODE; repos sync",
            "unset CLAUDECODE; ls src/test/fixtures/repos",
            "CLAUDECODE= cargo test repos",
            "unset CLAUDECODE; cargo test; repos",
        ] {
            passes(c);
        }
    }

    #[test]
    fn hook_input() {
        let input = |tool: &str, command: &str| {
            serde_json::json!({
                "session_id": "s",
                "hook_event_name": "PreToolUse",
                "tool_name": tool,
                "tool_input": {"command": command, "description": "d"},
            })
            .to_string()
        };
        assert_eq!(
            check_pre_tool_use(input("Bash", "git -C x push").as_bytes()),
            Some(Denial::GitPush)
        );
        assert_eq!(
            check_pre_tool_use(input("Bash", "git status").as_bytes()),
            None
        );
        assert_eq!(
            check_pre_tool_use(input("Write", "git push").as_bytes()),
            None
        );
        for bad in [
            &b"not json"[..],
            b"",
            b"{}",
            b"[]",
            br#"{"tool_name":"Bash"}"#,
            br#"{"tool_name":"Bash","tool_input":{}}"#,
            br#"{"tool_name":"Bash","tool_input":{"command":7}}"#,
            br#"{"tool_name":7,"tool_input":{"command":"git push"}}"#,
        ] {
            assert_eq!(
                check_pre_tool_use(bad),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn hook_output_is_the_deny_claude_code_reads() {
        let out: Value = serde_json::from_str(&Denial::GitPush.hook_output()).unwrap();
        assert_eq!(
            out,
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": Denial::GitPush.reason(),
                }
            })
        );
    }
}
