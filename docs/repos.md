# The `repos` tool

`repos` reports and converges the git state of every repo a `repos.toml`
registry declares, and is the gateway agents push through. It lives in this
repo as the `crates/fuz_repos` crate (a library plus the `repos` binary), a
Cargo workspace beside the SvelteKit app; gro never invokes cargo.

What it may write, what it never does, and how it divides the work with the
TS tasks and gro: [CLAUDE.md](../CLAUDE.md#scope-and-boundaries). This doc is
the per-command reference.

## Table of Contents

- [Install](#install)
- [Commands](#commands)
- [Finding the registry](#finding-the-registry)
- [`repos status`](#repos-status)
- [At rest](#at-rest)
- [`repos status --brief`](#repos-status---brief)
- [Fetching](#fetching)
- [Busy detection](#busy-detection)
- [`repos sync`](#repos-sync)
- [`repos push`, the gateway](#repos-push-the-gateway)
- [Third-party references](#third-party-references)
- [Cloning missing entries](#cloning-missing-entries)
- [Exit codes](#exit-codes)
- [Versions](#versions)
- [Testing](#testing)

## Install

```bash
cargo install --path crates/fuz_repos --locked # install the `repos` binary
```

`rust-toolchain.toml` pins the toolchain (rustup fetches it on first build),
and git must be 2.44 or newer (`GIT_NO_LAZY_FETCH` keeps a local `status` on
a partial clone off the network). It's Unix-only, and busy detection reads
`/proc`, so it works on Linux alone; elsewhere, with any session recorded, it
fails closed.

## Commands

```bash
repos status                 # git state of every repos.toml entry, local refs only, plus unregistered clones
repos status gro .           # narrow to targets: a key, a dir name, or a path inside a checkout (each names its entry); a named reference previews its refresh
repos status --verbose       # plus stash counts, unscoped sessions, each dirty worktree's own uncommitted item, and a block per entry and unregistered dir
repos status --json          # the versioned report
COLUMNS=80 repos status      # text wraps at COLUMNS (100 when unset or under 40); color only on a terminal without NO_COLOR
repos status --fetch         # fetch owned entries (and references asked for) from origin first (writes remote-tracking refs), and check private repos
repos status --references    # preview refreshing every third-party reference, as sync --references would (no targets with it)
repos status --jobs 4 --timings # parallelism (default 16), and per-phase timings on stderr
repos status --brief [<path>] # one line on the checkout holding the path (default: the cwd), or nothing — a SessionStart hook's nudge
repos sync                   # fetch as status --fetch does, then fast-forward, move, push, and clone what's safe; report outcomes
repos sync gro --json        # narrowed to targets; --json prints the versioned outcome report
repos sync typescript prettier # a named third-party reference is refreshed: fetched over HTTPS, then ff'd or moved where clean
repos sync --references      # refresh every third-party reference too (never a pin); alone — with targets it's a usage error
repos push                   # the gateway: fetch, then push the branch checked out here as a fast-forward of what was fetched; exit 1 unless it ends in sync
repos push app ../wt --json  # targets: a key or dir name (the entry's own checkout), or a path (the checkout holding it); --json prints the versioned outcome report
repos push --new-branch      # the user's (refused under CLAUDECODE): create the branch on origin when it has no upstream there, and track it as git push -u does
repos --version              # the crate version, the commit the binary was built from, and each --json document's format version
repos --registry <file> --root <dir> status # a registry kept outside the workspace

cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
UPDATE_GOLDEN=1 cargo test --test golden # regenerate the --json golden fixtures in src/test/fixtures/repos_status/ (never hand-edit)
```

## Finding the registry

`repos` finds its registry by walking up from the cwd (its physical path) to
the first `repos.toml` (from a linked worktree outside the workspace, it walks
up from the repo's main checkout instead), and the **workspace root is the
directory holding it as found** — entry dirs resolve against that root —
except inside a checkout (the nearest `.git` at or above it): there the root
is the nearest directory above that checkout whose `repos.toml` is the same
file (same device and inode, through symlinks; unreadable ones passed over).

So a registry kept in one of the workspace's own repos, with a `repos.toml`
symlink to it at the workspace root, roots the workspace at the link's dir
from inside that repo too, and from its linked worktrees, whose own committed
copy defers to the main checkout's. Nearest, so the innermost workspace wins:
a stray link further out never captures it, a link inside the repo doesn't
root it, and a different registry above neither roots nor stops the search.

A root found this way that is still in a checkout whose `origin` names one of
the registry's entries (no link at the root yet, say) is refused
(`root_in_entry`, exit `2`) — rooted there, a sync would clone the fleet into
that checkout. `--registry` naming a file makes _its_ directory the root as
given, with no search or check, unless `--root <dir>` names it; `--root`
alone skips the check too.

## `repos status`

`repos status` reports every registry entry's branches and their relation to
origin, uncommitted work in each checkout (linked worktrees too), what needs a
human, which checkouts another live Claude Code session is working in, and the
clones at the workspace root the registry doesn't name, grouped by what to do
next. It reads local refs alone; `--fetch` refreshes them first (see
[Fetching](#fetching)). Without `--fetch` it writes nothing: optional locks
are off (`GIT_OPTIONAL_LOCKS=0`), and so is lazy fetching.

How fresh the remote view is (`fetched_at`, the footer's oldest and its
`never` count) is the newest non-empty `FETCH_HEAD` across the repo's git
dirs; a repo with no `FETCH_HEAD` at all — a fresh clone writes none — is
dated by git's own `clone: from` entry, the first line of its `logs/HEAD`
(none for a clone of an empty repo, or one whose refs are in the reftable
format, which reads as never fetched). An empty `FETCH_HEAD` means the last
fetch failed, so its age is unknown and reads as never fetched, clone or not.

In the default view each entry's `uncommitted` item totals its primary
checkout's dirt, then names its one other dirty worktree, or folds several
into a count with their summed dirt; `--verbose` lists every dirty checkout
with its dirt by kind.

## At rest

Each entry's `at_rest` in the `--json` report says whether its own checkout
— the primary, `checkouts[0]` — sits where the registry puts it. It's
decided with the verdicts, so a consumer reads readiness rather than
re-deriving it from the checkout and its branches:

- `on_branch` — HEAD is on the branch the entry follows (its `branch`);
  `false` when detached or on another branch, `null` when the entry follows
  none (a reference declaring no `branch`)
- `clean` — nothing staged, unstaged, untracked, or conflicted
- `idle` — no operation in progress
- `followed` — the followed branch's relation to origin, as its entry in
  `branches` carries it; `null` when the entry follows no branch, has no
  local branch of that name, or its branches aren't compared against a
  remote — a third-party reference the run doesn't refresh, whose branches
  with local work read `untracked` for want of a comparison

`at_rest` itself is `null` exactly when `checkouts` is empty: the entry is
missing or not a repo, or its probe failed (`probe_error`). A pin's facts are
decided as any entry's; that it's pinned is its own field. The text
summary's `clean · on branches · pinned` counts read `on_branch`. The gitops
tasks read readiness from these facts: `gro gitops_publish --wetrun` refuses
a repo not at rest (see [Publishing](publishing.md#readiness)),
`gro gitops_sync` refuses one off its branch, dirty, or mid-operation (see
[Troubleshooting](troubleshooting.md)), and the diagnostics list them.

Readiness wants `followed` too: a followed branch that's unborn (no commit
yet) reads `on_branch` true, clean, idle, and `followed` `null`, beside a
`default_branch_missing` reason unless the entry is pinned. `at_rest` says nothing of live
sessions — `checkouts[0].busy` does. And `followed` is as fresh as the
remote-tracking refs, as `branches` is: as of `fetched_at`, and stale after
a failed `--fetch`.

A failed probe's `probe_error` is an object: its `kind` — the entry's path
couldn't be looked up (`path_unreadable`) or isn't UTF-8
(`non_utf8_path`); a read with a kind of its own failed, however it failed
(`config_unreadable`, `fetch_url_unreadable`, `push_urls_unreadable`); or
another git call couldn't start (`git_not_run`), timed out
(`git_timed_out`), failed (`git_failed`), or printed what the probe can't
read (`unexpected_output`) — and a `message` for display. The text output
prints the message. A HEAD that can't be read — an unprobed worktree's, or
an unlisted git dir's — is `null`; a readable one is `branch` or
`detached`, as a probed checkout's is.

## `repos status --brief`

`repos status --brief [<path>]` is the nudge a user-scope `SessionStart` hook
runs as `repos status --brief "$CLAUDE_PROJECT_DIR"`: its stdout lands in the
new session's context, so it prints nothing unless the checkout holding the
path has something that session should know, and one plain line otherwise —
`repos: <key> — …` naming, in order:

1. the other live sessions working in that checkout itself (placed there by
   where they are or by the lock Claude Code put on it for them — not a
   session elsewhere in the repo, which sync still counts busy in the
   `.claude/worktrees/` checkouts, for the subagents it may have there)
2. an operation in progress there
3. its branch behind or diverged from its origin upstream (with how long ago
   the repo was fetched)
4. ahead of it (unpushed)

Behind and ahead are said only for an owned entry that isn't pinned, and dirt
never (the session sees its own working tree). It probes that entry alone,
from local refs — no fetch, no unregistered scan, nothing written — and finds
the registry walking up from the path (its physical path, as for the cwd)
rather than the cwd. The caller is excluded from the sessions as anywhere
(`CLAUDE_PID`, which Claude Code sets for its hooks too), and with busy
detection unavailable it says nothing of sessions.

It never fails its hook: no registry, a refused root, a path in no entry (the
workspace root, an unregistered clone, outside the workspace), git missing, or
a failed probe all exit `0` in silence; only a flag it can't take (`--json`,
`--fetch`, `--verbose`, `--references`) or a second path is a usage error,
exit `2`.

## Fetching

`status --fetch` fetches owned entries (and the references a run refreshes)
from origin before probing; `sync` and `push` run the same fetch first.

**Failures.** Each failed fetch gets a kind (`ref_gone`, `unreachable`,
`repo_not_found`, `timed_out`, …, else `failed` with git's line).

**Visibility.** Each `[repos]` entry declared private gets an anonymous `git
ls-remote` of its HTTPS URL with no credential in reach — readable means it
leaked, printed first as `visibility`.

**Confinement.** The fetch writes remote-tracking refs, the objects behind
them, `FETCH_HEAD`, and a shallow boundary, and nothing else — never a tag, a
submodule, or a commit-graph — whatever the repo's config says; an entry
whose refspecs could write outside `refs/remotes/origin/`, or another
remote's into it, isn't fetched, and
neither is an owned one whose fetch wouldn't reach the registry's repo as git
resolves origin's URL, `insteadOf` applied: the fetch would bring in another
repo's history. An origin set to another URL says so on its origin-drift
line; one set to the registry's that a rewrite sends elsewhere is a
needs-human `fetch_url_mismatch` naming the rewrite; and one spelled otherwise
that a rewrite sends to the registry's repo (an alias, `gh:me/app`) is
fetched, its drift still holding the rest.

**URLs.** Origin URLs are redacted wherever shown, and registry URLs are
strict (a plain DNS host, no userinfo or port). An origin or push URL names
the registry's repo only when read as git connects for it: the host (to the
first `/` after `scheme://`, or the first `:` in scp-like `user@host:path`) is
the registry's, with no port, and the path on it is `<account>/<name>`, case
folded, a `.git` or trailing `/` dropped — an `@` past the host, an escape
(git decodes those first), a bracket anywhere (git unwraps one into the host),
a user other than ASCII letters, digits, and `._+-`, or an IP literal never
matches.

The details live in the rustdoc of `remote.rs`, `probe.rs`, and `url.rs`.

## Busy detection

**What it reads.** Busy detection reads the live Claude Code sessions under
`$CLAUDE_CONFIG_DIR` and `~/.claude` — `sessions/<pid>.json` and the daemon
roster's workers, each verified against `/proc/<pid>/stat`'s start time —
excluding the caller (`CLAUDE_PID`, honored only when it's an ancestor of the
process).

**Where a session works.** A session works at its recorded cwd (Claude Code's
`originalCwd`, which entering or exiting a worktree rewrites), a roster
worker's `worktreePath`, and its process's current cwd (`/proc/<pid>/cwd`).
It marks busy:

- the checkout each of those sits in
- the checkout whose git dir the nearest `.git` above it names (so a worktree
  moved or copied by hand is busy wherever its files are)
- every checkout under the repo's main checkout's `.claude/worktrees/`
  (Claude Code's subagent worktrees, whose sessions keep the parent's cwd;
  for a bare or `--separate-git-dir` repo, the common dir's too, and for a
  moved worktree, its own)
- every checkout whose lock names it — Claude Code locks each worktree it
  creates with the reason `claude <agent|session> <name> (pid <pid> start
  <start>)`, matched against the session's pid and start time

A busy checkout holds every action on its branches, pushes included.

**What it can't see.** Claude Code roots agent worktrees at its tracked cwd,
which the Bash tool's `cd` moves without moving the process or the session
file, so such a worktree is caught by its lock alone; one Claude Code doesn't
lock (a `WorktreeCreate` hook's, another tool's), and work through `GIT_DIR`
or `git -C`, are placed by those paths alone. A Claude process that writes no
session file (an agent-team teammate, a session started inside another's
environment) or runs under a `CLAUDE_CONFIG_DIR` the tool doesn't read is
invisible, its locks with it, and a change to Claude Code's lock format
silently drops the lock signal.

**It fails closed.** A live session it can't vouch for (a file that won't
parse, another machine's or pid namespace's, no `/proc`, a path it can't
resolve), a file or dir it can't read, a relative `CLAUDE_CONFIG_DIR` or
`HOME`, or `HOME` unset makes detection unavailable, which holds every action
and prints on the `failed` line. A checkout whose path can't be resolved may
be busy: it holds the branches checked out there and shows as a `needs_human`
reason. So does a git dir no worktree list names that shares the repo's refs
(a hand-made `commondir`, or `git-new-workdir`) with a session in it, and a
branch git says is checked out in a worktree the probe didn't find. Sessions
in no checkout show only under `--verbose` and in the JSON.

The details live in the rustdoc of `sessions.rs` (the reader) and `busy.rs`
(the scoping).

## `repos sync`

`repos sync` is `status --fetch` followed by acting on each branch's verdict.
Beyond the fetch, it writes the branch it acts on, the checkout that branch is
on, the remote branch a push moves (and its remote-tracking ref), and new
clones.

**Fast-forwards and moves.** A branch behind is fast-forwarded — in place when
no checkout has it (a confined `git fetch .` of the exact upstream commit, so
git refuses a non-ff and a branch checked out anywhere), in its checkout with
`merge --ff-only --no-overwrite-ignore` only when that checkout is still on it
and clean — and a shallow branch with no local commits moves to the fetched
tip (`update-ref` compare-and-swap in place, `switch -C --no-overwrite-ignore`
in a clean checkout).

**Re-checks.** The live sessions are read after the fetch and again right
before each action, and each action re-checks what it relies on (and checks
after the fact what git can't refuse); git refusing is `failed`, exit `1` (as
is a failed probe, or a fetch that failed or that the tool refused to run). A
fast-forward in a checkout moves whatever branch HEAD is on when git runs, so
a branch switched in the instant after sync read the checkout may move forward
instead; the action then fails, naming it.

**Pushes.** A branch ahead is pushed to its upstream's branch on the
registry's repo — `git send-pack` of the commit classified straight to the
registry's SSH URL, never through `origin`, so no `insteadOf`,
`pushInsteadOf`, or `remote.<url>` config written in the meantime can redirect
it; under a lease on the fetched tip (`--force-with-lease=<ref>:<fetched>`),
no tags or push options, SSH only — once the branch still reads as
classified, origin's push URL (`git remote get-url --push --all`, `pushurl`
and `pushInsteadOf` applied) is exactly the registry's repo over SSH, and the
fetched tip is still an ancestor of the commit, the same count behind it.

The lease is a compare-and-swap, never a force over unseen work: git refuses
unless the remote's branch is exactly what the fetch saw, and the ancestor
check keeps the push a fast-forward. Any other push URL holds its pushes as a
`needs_human` reason, and an upstream at `origin/HEAD` (or outside
`refs/heads/`) is left to a person. A remote branch moved or deleted since the
fetch fails the lease (`held`, rerun — a deleted one then reads gone, so no
push recreates it but the user's `repos push --new-branch`, and only while it
has commits on no remote); the remote's own refusal or an unreachable host is
`push_failed`, exit `1`. After a push (or finding the commit already there,
another hand's push since the fetch) the remote-tracking ref moves to the
commit by compare-and-swap on the fetched tip, so `status` reads the branch in
sync without a refetch.

**An agent's sync pushes as a person's does**: under `CLAUDECODE` (Claude
Code's agent shells) nothing is held for being an agent's — every branch ahead
that nothing else holds is pushed, commits other, finished sessions made
included — and busy detection keeps it off the checkouts live sessions work
in.

**What it leaves.** A branch that's a symbolic ref never acts. It never
rebases, merges anything but a fast-forward, deletes a branch, or prunes a
worktree (the fetch prunes only remote-tracking refs gone upstream), and never
touches a pin. A branch whose upstream is gone reads as cleanup, to delete by
hand, except the branch the entry follows: its upstream gone (the remote's
default renamed, say) needs a person. A failed fetch holds that entry's moves
and pushes (its remote-tracking refs weren't refreshed), and a branch on HEAD
in several checkouts holds its fast-forward or move.

The rustdoc of `sync.rs` has the details.

## `repos push`, the gateway

`repos push` is what an agent runs instead of `git push`.

**Targets.** Without targets it pushes the branch checked out in the checkout
holding the cwd; a target is a registry key or dir name (the entry's own
checkout) or a path (the checkout holding it, a linked worktree's own).

**Pipeline.** For those entries alone it runs what sync runs before acting —
the same fetch, a re-probe, the live sessions read (the caller's own
excluded), classification — then acts on each checked-out branch's push
verdict alone, through sync's own push (the lease, the direct URL, the
compare-and-swap, every re-check right before). It never fast-forwards,
moves, clones, or touches another branch: a branch behind is reported for
`repos sync` to fast-forward, a diverged one left to a person.

**Policy.** The policy is structural: owned entries only (a third-party
reference or a pin named is a usage error), never a force or a tag; another
live session in the checkout holds the push (`busy`), and so does origin
drift; a failed fetch or an entry-level reason (origin drift among them)
holds even a branch that reads in sync, since its refs may not be origin's (a
branch with no upstream configured reads `no_upstream` whatever the fetch,
since that's its config); dirt doesn't matter, since a push moves refs alone.
It runs for an agent as for a person.

### `--new-branch`

A branch with no upstream on origin reads `no_upstream`: **creating the
remote branch is the user's**, with `repos push --new-branch`, which an
agent's shell (`CLAUDECODE`) is refused, exit `2`.

It creates a branch with no upstream configured, or whose same-named upstream
on origin is gone while it has commits on no remote (with none — merged, say
— `no_upstream`, recreated by hand only; never the branch the entry follows,
whose upstream gone is the remote's default renamed or deleted:
`no_upstream`, repointed by hand), as `refs/heads/<b>` on the registry's repo
— the same send-pack, under a lease that no such ref exists
(`--force-with-lease=<ref>:`), so one created there since the fetch is held,
never overwritten — then, as `git push -u`, the remote-tracking ref by
compare-and-swap on none and `branch.<b>.remote`/`.merge`, and reads
`created`.

A branch with a live upstream pushes as without the flag; one tracking
another remote, or origin's branch under another name, stays `no_upstream`;
one origin already has at another commit reads `remote_branch_exists` (never
adopted: set the upstream by hand), and one the fetch refspec leaves out
`needs_human` (`unmapped`). A run stopped between creating the branch and
setting its upstream is finished by the next `--new-branch`: the branch is on
origin at the very commit, the lease reads it up to date, and the upstream is
set.

### Push outcomes

Exit `0` when every target's branch ends in sync with its upstream (pushed,
created, or already there), `1` when any didn't push (held, behind, diverged,
detached, no upstream, a remote branch in the way, a checkout not read, a
failed fetch or push), as `git push` exits on a rejected ref, and `2` for
usage (an unknown target, the cwd in no entry's checkout, a third-party or
pinned target, `--new-branch` in an agent's shell). `--json` prints its own
versioned outcome report: the targets' entries after the fetch, and one
outcome per target checkout.

The rustdoc of `push.rs` has the details.

### Agents push through `repos push`

The recommended Claude Code settings deny raw `git push` (a `Bash(git push:*)`
prefix rule, with `Bash(repos push --new-branch:*)` beside it) and allow
`Bash(repos:*)`, and agents are instructed to push with `repos push`.
Permission rules hold in every permission mode but match a command's prefix
alone, so a push spelled another way slips past them: guidance, not a
boundary — the host's own rules are the floor.

## Third-party references

**Third-party references are like locked dependencies**: left as they are —
never fetched, no branch compared against a remote, only local work reported
— unless the run names them as targets (a path inside a checkout names its
entry) or passes `--references`, which takes no targets (with them it's a
usage error, exit `2`).

**Refresh.** Then each is refreshed (its `refresh` verdict `act`): fetched
from origin over HTTPS alone, with the same confined fetch, and each branch
fast-forwarded, or moved when shallow, as an owned one would be where clean;
never pushed, so a branch ahead is local-only work, and one diverged or
shallow with local commits is left to a person. `status` takes the same
targets and `--references` to preview a refresh from local refs, and fetches
it under `--fetch`.

**What holds a refresh.** A pin is never fetched; named, it's refused
(`refresh` held `pinned`), and `--references` passes it over. A reference
whose `origin` isn't the registry's repo (a fork, or no URL) is never fetched:
its refresh is held (`refresh held (origin drift)`, held by `entry`) and its
origin-drift line says the fix. A refresh also needs an HTTPS origin naming
the registry's repo, as git resolves it (`insteadOf` applied): the same repo
over SSH, `http://`, or `git://`, or a rewrite of it, holds the refresh
(`refresh held (origin not HTTPS)`, held by `origin_not_https`), never
fetched, with a needs-human line saying the `set-url` fix or naming the
rewrite.

**Partial clones.** A partial clone (a `sparse` reference, cloned
`--filter=blob:none`) lacks the blobs a new tip's checkout needs: a
fast-forward or move in its checkout fetches them on demand from origin
alone, writing objects and no ref, over the one transport origin's URL
names as git resolves it (`insteadOf` applied; SSH or HTTPS, whoever owns
the repo — an owned partial clone resolving to neither is a needs-human
`fetch_url_mismatch`), and only when no other remote is a promisor — both
read again right before, and the action held (`changed`) if origin no longer names the registry's repo over
that transport or another promisor appeared; every other call keeps lazy
fetching off.

## Cloning missing entries

**Each missing entry is cloned** — agents' runs included, and whether or not
busy detection can vouch for every session, since a clone only creates a
dir.

**The recipe.** Owned entries clone over SSH, third-party ones over HTTPS,
each allowed that transport alone; on the entry's `branch` (`--branch`, else
the remote's default), `--depth 1` when `shallow`, cone-mode `sparse` with
`--filter=blob:none`, with `--no-tags` and no submodules or hooks — the
`tagOpt` that records is then unset, so the user's own `git fetch` there
follows tags as in any clone. A `sparse` path is checked at parse: plain
relative directory names, no globs.

**Placement.** The clone is made in a temp dir beside the entry's
(`.<dir>.repos-clone-<pid>-<nonce>`, the nonce random per process, so runs in
separate pid namespaces never share one) and moved into place only when
whole, claiming the path so nothing there is ever cloned over; a failure or
timeout deletes the temp dir, and the unregistered scan names any it finds as
an unfinished clone — one a killed run left, or one still running, to remove
once no `repos sync` is.

**What holds a clone.** Anything at the path — a file, an empty dir, a
dangling symlink — reads as not a repo, never missing; a live session at or
under the path, or another entry's gone worktree recorded there, holds the
clone, and an entry whose `url` another entry shares is never cloned (its dir
may have been a worktree of that repo) — a `needs_human` reason, as is a
missing entry whose repo an unregistered dir at the root already clones (its
origin names it, or a rename of it differing only in case and `-` against
`_`: likely the entry's checkout under another name). The unregistered scan
runs for that whenever the run includes a missing entry, named or not, before
anything is cloned, in `sync` as in `status` (a run with targets doesn't
report what it found); a clone whose origin names the repo by an unrelated
old name isn't caught.

**Read back.** The clone is read back in place (on its branch, tracking
origin's, clean) and reported `cloned`, or `clone_failed` (classified as a
fetch failure is) or `failed`, exit `1`.

The rustdoc of `clone.rs` has the recipe.

## Exit codes

- `0` when the command ran — what the report says is data, not failure
- `1` for a runtime failure: under `sync`, anything that failed (a fetch, git's
  failure or the tool's refusal to run one whose refspec it can't confine; a
  probe; an action git refused); under `push`, any target whose branch didn't
  end in sync with its upstream ([Push outcomes](#push-outcomes)); or,
  under any command, a fatal I/O error
- `2` when the caller must change something — usage, a missing or invalid
  registry, git missing or too old, an unknown target, a refused root
  (`root_in_entry`), and `push`'s usage cases

A fatal error prints `error: …` and `hint: …` on stderr; under `--json` it
also prints one error document on stdout, in place of the report.

## Versions

`repos --version` prints one line: the crate version, the commit the binary
was built from, and the version of each `--json` document it prints —

```
repos <crate> (<commit>[, dirty]) · formats: status <n>, sync <n>, push <n>
```

Each document carries its own as `version` (`sync` and `push` embed a status
report, which carries the status one). A version is bumped on any change to
its document's shape, new fields and variants included, since consumers
parse with strict objects and closed unions; an absent value is `null`, never
an omitted key; the sync and push versions move
with every status bump. The crate version doesn't track the formats, so a
consumer checks the one it parses.

## Testing

`cargo test --workspace` runs integration tests over hermetic fixture
workspaces (`crates/fuz_repos/tests/support`): real repos in a tempdir, each
cloned from a local bare remote, with git's environment cleared (no global or
system config, fixed identities and dates) and no network:

- the `ssh` on `PATH` is the fixture's own, serving fetches and pushes to the
  registry's SSH URLs from the local bare remotes and refusing anything else,
  and SSH failures come from fakes too
- `GIT_EXEC_PATH` is the fixture's — git's own programs, but a
  `git-remote-https` that refuses every URL unless a test serves the bare
  remotes through it (`serve_https`)
- the visibility check reads `file://` repos and a loopback HTTP server
- busy detection reads fixture config dirs whose session files name the
  tests' own child processes

The test files split by command and aspect: `status_*.rs`, `sync.rs` and
`sync_*.rs`, `push_*.rs`, `cli_*.rs` (the binary's documents, text, and exit
codes), `targets.rs`, `registry_real.rs`, and `golden.rs` with its
`golden/` modules. Helpers a family shares sit beside `support/mod.rs` in
`support/` (`busy`, `cli`, `push`, `remote`, `sync`, `unregistered`,
`worktrees`).

The tests need git 2.44 or newer on `PATH`, and Linux (`/proc`,
`/etc/machine-id`). CI runs these fmt, clippy, and test commands (with
`--locked`, and `--no-fail-fast` on tests) in the `rust` job of
`.github/workflows/check.yml`, beside the gro check.

`src/test/fixtures/repos_status/*.json` are the `repos status --json`,
`repos sync --json`, and `repos push --json` golden documents (report,
narrowed report, busy-detection states, sync report, push report, error
documents), written by `crates/fuz_repos/tests/golden.rs` as the contract TS consumers parse
against — regenerate them with `UPDATE_GOLDEN=1 cargo test --test golden`,
never by hand. Each error `repos status --json` can print has its own
document, `error_report_<kind>.json`; `sync_error_report.json` and
`push_error_report.json` show the same document shape at those commands'
versions. The status documents' TS mirror, `src/lib/repos_status.ts`, is
checked by parsing every status golden with its strict schemas
(`src/test/repos_status.golden.test.ts`); the sync and push documents have
no TS consumer yet.

The goldens hold to two checks. Every report they build is checked for its
structure (`crates/fuz_repos/tests/golden/invariants.rs`): keys and dirs
unique, nothing read of an entry with no repo or a failed probe, the
primary checkout first, an unasked reference's branches its local work
alone, one default-branch reason at most, no session placed while busy
detection is unavailable, relations that fit the clone's depth, and no
fetch time beside a fetch that failed and emptied `FETCH_HEAD`. They re-derive none of
`classify`'s decisions: the integration tests over real git pin those. And
together the goldens cover every variant
(`crates/fuz_repos/tests/golden/coverage.rs`): the status documents —
both reports, `sessions.json`, and the status error documents — carry every
variant of every closed enum the status report and its error document
hold, in each place it can appear (each action's holds — a branch's, a
refresh's, a clone's — their own enum), the sync and push documents
every outcome of theirs, and the sync document every hold sync can
report. Each enum's variants are listed once, a list an exhaustive
`match` checks, and the floor counts that list: a new variant fails to
compile until it's listed, and fails the floor until a golden covers it.
