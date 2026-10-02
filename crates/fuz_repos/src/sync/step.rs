//! One sync action's git calls: the fast-forwards, shallow moves, pushes,
//! branch creation, and a partial clone's lazy fetch — and, in the `rebase`
//! module beside this one, a rebase's replay and move. What each relies on,
//! and the races it leaves, is the `sync` module's doc.

use std::path::Path;
use std::time::Duration;

use crate::classify::{lazy_transport, origin_matches, push_urls_match};
use crate::git::{CallOptions, Git, GitError, GitOutput, NetworkOptions};
use crate::porcelain::{self, ConfigFacts};
use crate::probe::{
    ConfigReadError, STATUS_ARGS, read_config, read_fetch_url, read_push_urls, read_shallow_roots,
};
use crate::registry::RepoUrl;
use crate::remote::{RefspecContext, RemoteFailure};
use crate::report::BranchSyncHold;
use crate::state::Head;

/// The timeout for an action that rewrites a working tree (`merge
/// --ff-only`, `switch -C`), in place of `LOCAL_TIMEOUT`.
///
/// A checkout of a large tree through the user's filter drivers (Git LFS
/// fetching content) can take minutes, and git killed mid-checkout leaves
/// the files half-written against an unmoved HEAD. Long enough for any
/// checkout that is making progress; a hung filter still ends. A rebase's
/// replay runs under it too: a long range merges commit by commit, and one
/// killed part-way has moved nothing.
pub(super) const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How a fast-forward or shallow move went, short of failing.
#[derive(Debug)]
pub(super) enum UpdateDone {
    /// The branch moved from `from` to `to`.
    Updated { from: String, to: String },
    /// The branch already held the tip.
    AlreadyThere,
    /// A re-check held it.
    Held(BranchSyncHold),
}

/// How a push went, short of failing (`Actor::push`).
#[derive(Debug)]
pub enum PushDone {
    /// The remote's branch moved from `from`, the fetched tip, to `to`.
    Pushed { from: String, to: String },
    /// The remote's branch already held the commit.
    AlreadyThere,
    /// Held, or failed at the remote.
    Stopped(Stop),
}

/// Why a push or a branch's creation stopped short of the remote taking
/// it: a re-check held it, or the remote refused it or couldn't be
/// reached.
#[derive(Debug)]
pub enum Stop {
    /// A re-check held it, or git refused it for a remote that moved since
    /// the fetch (`rejected`).
    Held(BranchSyncHold),
    /// The push failed at the remote, or reaching it.
    PushFailed(RemoteFailure),
}

/// The upstream a branch has when `repos push --new-branch` creates it on
/// origin (`creatable`): it decides what the creation re-checks and whether
/// it sets one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewBranchUpstream<'a> {
    /// None configured (`branch.<b>.merge` unset): creating the branch sets
    /// one, as `git push -u` does.
    Unset,
    /// Origin's branch of the same name, gone there: `tracking`, its
    /// resolved remote-tracking ref (`refs/remotes/origin/<b>`), is the ref
    /// the creation writes, and the upstream config stays as it is.
    Gone { tracking: &'a str },
}

/// The variables `Step::mapped_upstream`'s `--config-env` reads the
/// upstream it tries from.
const UPSTREAM_REMOTE_VAR: &str = "REPOS_UPSTREAM_REMOTE";
const UPSTREAM_MERGE_VAR: &str = "REPOS_UPSTREAM_MERGE";

/// One action's git calls, on `branch`: a fast-forward or move toward the
/// upstream tip it's given, a rebase onto it (`Rebase`), a push (`Push`), or
/// a creation (`NewBranch`).
pub(super) struct Step<'a> {
    git: &'a Git,
    pub(super) opts: CallOptions<'a>,
    branch: &'a str,
    /// The branch's ref, `refs/heads/<b>`.
    pub(super) local: String,
    common_dir: &'a Path,
    /// A partial clone's lazy fetch, for the actions that rewrite a working
    /// tree (`run_checkout`); `None` keeps it off.
    lazy: Option<LazyFetch<'a>>,
}

/// How an action that rewrites a partial clone's working tree fetches the
/// objects it lacks: from its promisor remote, origin — only when no other
/// remote is one (`ConfigFacts::other_promisor`), since git asks each in
/// turn — over `transport` alone (`lazy_transport`), and only while origin
/// still reaches `repo` over it (`Step::lazy_origin_moved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LazyFetch<'a> {
    pub(super) transport: &'static str,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    pub(super) batch_ssh: bool,
    /// The registry's repo: what origin must name when the fetch runs.
    pub(super) repo: &'a RepoUrl,
}

/// A partial clone's lazy fetch, from its `config`: `None` — lazy fetching
/// stays off, and a checkout needing a missing blob fails — for a repo
/// that isn't a partial clone, one with another promisor remote, and one
/// whose origin, as git resolves it (`ConfigFacts::origin_fetch_url`,
/// rewrites applied: where the fetch connects), names no transport the
/// fetch may take (`lazy_transport`), or wasn't read. `env_ssh`: the
/// environment configures SSH; `repo`: the entry's.
///
/// Only when classify let the branch act, so with that URL naming the
/// registry's repo over SSH or HTTPS (`fetch_url_mismatch` holds an owned
/// entry, and a reference's refresh is held unless it's HTTPS) — as the
/// probe read it: right before the checkout, it's read again, and so is
/// whether another promisor remote is configured
/// (`Step::lazy_origin_moved`).
pub(super) fn lazy_fetch<'a>(
    config: &ConfigFacts,
    env_ssh: bool,
    repo: &'a RepoUrl,
) -> Option<LazyFetch<'a>> {
    if config.partial_filter.is_none() || config.other_promisor {
        return None;
    }
    Some(LazyFetch {
        transport: lazy_transport(config.origin_fetch_url.as_deref()?)?,
        batch_ssh: config.batch_ssh(env_ssh),
        repo,
    })
}

/// The confined local fetch's flags before `.` and its refspec: what the
/// remote fetch (`probe`'s `FETCH_ARGS`) switches off, plus `FETCH_HEAD`.
/// `--porcelain` prints what moved, old and new ids exact.
const LOCAL_FETCH_ARGS: [&str; 10] = [
    "-c",
    "fetch.bundleURI=",
    "fetch",
    "--porcelain",
    "--no-write-fetch-head",
    "--no-tags",
    "--no-prune",
    "--no-prune-tags",
    "--recurse-submodules=no",
    // no `--update-head-ok`: git refuses a branch checked out anywhere
    "--no-write-commit-graph",
];

/// What a push sends, as classified.
pub(super) struct Push<'a> {
    /// The commit the branch held when probed, or the tip a rebase just
    /// moved it to: the one pushed, whatever lands on the branch after.
    pub(super) oid: &'a str,
    /// The resolved upstream ref, under `refs/remotes/origin/`: the fetched
    /// tip, and the ref that records the push (`record_push`).
    pub(super) upstream: &'a str,
    /// The upstream's ref on origin (`push_target`), named explicitly, and
    /// the ref the lease is on.
    pub(super) target: &'a str,
    /// The commits ahead the verdict counted.
    pub(super) commits: u32,
    /// A shallow clone, where the verdict counted commits on no remote ref.
    pub(super) shallow: bool,
    /// The registry's repo: where the push goes (its SSH URL), and what
    /// origin's push URL must name.
    pub(super) url: &'a RepoUrl,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    pub(super) batch_ssh: bool,
}

/// What a new branch's creation sends, as classified (`Step::create`).
pub(super) struct NewBranch<'a> {
    /// The commit the branch held when probed: the one the remote branch
    /// is created at.
    pub(super) oid: &'a str,
    /// The upstream it has: none, so creating it sets one, or origin's
    /// same-named branch, gone.
    pub(super) upstream: NewBranchUpstream<'a>,
    /// The registry's repo: where the branch is created (its SSH URL), and
    /// what origin's push URL must name.
    pub(super) url: &'a RepoUrl,
    /// Batch-mode SSH, unless the user configures SSH (as the fetch).
    pub(super) batch_ssh: bool,
}

/// One `git send-pack` of a commit to a ref on the registry's repo.
struct SendPack<'a> {
    oid: &'a str,
    /// The ref on the registry's repo, named explicitly.
    target: &'a str,
    /// What the lease expects the remote's ref to be: the fetched tip, or
    /// `None` for no such ref.
    expect: Option<&'a str>,
    url: &'a RepoUrl,
    batch_ssh: bool,
}

/// What a send-pack came to, short of failing.
#[derive(Debug)]
enum Sent {
    /// The remote's ref moved to the commit.
    Pushed,
    /// The remote's ref already held it.
    UpToDate,
    /// Refused or not sent: held, or failed at the remote (`rejected`).
    Stopped(Stop),
}

/// How creating a branch on the remote went, short of failing.
#[derive(Debug)]
pub(super) enum Creation {
    /// The remote branch is at the commit, and its upstream set.
    Created(String),
    /// Origin's fetch refspec maps the branch to no remote-tracking ref,
    /// so it couldn't track what it would create.
    Unmapped,
    /// The fetch found origin holding a branch by that name, at another
    /// commit (the one held): never overwritten, or adopted.
    Exists(String),
    /// Held, or failed at the remote.
    Stopped(Stop),
}

/// The push's command and flags before the lease, the URL, and the
/// refspec.
///
/// `git send-pack`, the plumbing under `git push`, because it connects to
/// the URL it's given as written. `git push <url>` reads a URL through the
/// remote config first: `url.<base>.insteadOf` and `pushInsteadOf` rewrite
/// it, and a `remote.<url>` section named by the URL itself takes it over
/// — from any config file, read when git runs, so a config written after
/// the push URL was checked could send the push elsewhere. Send-pack
/// consults neither, so the push reaches the registry's repo or fails.
///
/// And nothing but the one ref: send-pack pushes no tags
/// (`push.followTags` is `git push`'s), no submodules, and no push options
/// (it sends only the ones named with `--push-option`, never
/// `push.pushOption`), and runs no `pre-push` hook. `--no-signed`: no push
/// certificate (a signing prompt under a batch run, and a host that takes
/// none fails the push). `--receive-pack` names git's own remote command.
/// `--thin`, as `git push` packs. `--helper-status` prints one line per
/// remote ref on stdout (`pushed_ref`). SSH runs as the user configures it
/// (`core.sshCommand`, `~/.ssh/config`), their own program.
const SEND_PACK_ARGS: [&str; 5] = [
    "send-pack",
    "--helper-status",
    "--thin",
    "--receive-pack=git-receive-pack",
    "--no-signed",
];

impl<'a> Step<'a> {
    /// A step whose calls stop repo discovery at `root` and are local:
    /// lazy fetching off in every one but `run_checkout`'s, which lifts it
    /// by `lazy`.
    pub(super) fn new(
        git: &'a Git,
        root: &'a Path,
        branch: &'a str,
        common_dir: &'a Path,
        lazy: Option<LazyFetch<'a>>,
    ) -> Self {
        Self {
            git,
            opts: CallOptions {
                ceiling: Some(root),
                ..CallOptions::default()
            },
            branch,
            local: format!("refs/heads/{branch}"),
            common_dir,
            lazy,
        }
    }
}

impl Step<'_> {
    /// Runs git in `dir` (`Git::run`), a call that couldn't run failing
    /// with its message (`git_message`).
    pub(super) fn run(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<GitOutput, String> {
        self.git.run(dir, args, opts).map_err(|e| git_message(&e))
    }

    /// Git's stdout, from a call in `dir` that succeeded
    /// (`Git::output`); else git's message.
    fn output(&self, dir: &Path, args: &[&str], opts: CallOptions<'_>) -> Result<Vec<u8>, String> {
        self.git
            .output(dir, args, opts)
            .map_err(|e| git_message(&e))
    }

    /// Git's stdout as a string, from a call in `dir` that succeeded
    /// (`Git::output_string`); else git's message.
    pub(super) fn output_string(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<String, String> {
        self.git
            .output_string(dir, args, opts)
            .map_err(|e| git_message(&e))
    }

    /// Runs git in `dir` for an answer it gives by its exit code: its
    /// output for 0, `None` for 1 (the call's quiet "no"), and git's
    /// message for anything else.
    pub(super) fn answer(&self, dir: &Path, args: &[&str]) -> Result<Option<GitOutput>, String> {
        let out = self.run(dir, args, self.opts)?;
        match out.status.code() {
            Some(0) => Ok(Some(out)),
            Some(1) => Ok(None),
            _ => Err(first_message(&out.stderr)),
        }
    }

    /// The commit `rev` names in `dir`.
    pub(super) fn resolve(&self, dir: &Path, rev: &str) -> Result<String, String> {
        let rev = format!("{rev}^{{commit}}");
        self.output_string(
            dir,
            &["rev-parse", "--verify", "--end-of-options", &rev],
            self.opts,
        )
        .map(|s| s.trim().to_owned())
    }

    /// The commit the branch holds in `dir`, or `None` when the branch no
    /// longer exists (deleted since classifying); any other failure to
    /// resolve it stays an error.
    pub(super) fn resolve_local(&self, dir: &Path) -> Result<Option<String>, String> {
        let local = self.local.as_str();
        let err = match self.resolve(dir, local) {
            Ok(commit) => return Ok(Some(commit)),
            Err(err) => err,
        };
        let out = self.run(
            dir,
            &["show-ref", "--exists", "--end-of-options", local],
            self.opts,
        )?;
        // `--exists`: 2 for a ref that doesn't exist, and only for that
        if out.status.code() == Some(2) {
            Ok(None)
        } else {
            Err(err)
        }
    }

    /// Whether `ancestor` is `commit` or one of its ancestors.
    fn is_ancestor(&self, dir: &Path, ancestor: &str, commit: &str) -> Result<bool, String> {
        let answer = self.answer(dir, &["merge-base", "--is-ancestor", ancestor, commit])?;
        Ok(answer.is_some())
    }

    /// Whether the branch is a symbolic ref now, which a write to it would
    /// go through to its target.
    pub(super) fn is_symref(&self, dir: &Path) -> Result<bool, String> {
        // `-q`: 1 for a plain ref (or none), silently
        let answer = self.answer(dir, &["symbolic-ref", "-q", &self.local])?;
        Ok(answer.is_some())
    }

    /// Fast-forwards a branch no checkout has to `upstream`'s tip, with a
    /// fetch from the repo itself (the module doc says why).
    pub(super) fn ff_in_place(&self, dir: &Path, upstream: &str) -> Result<UpdateDone, String> {
        let to = self.resolve(dir, upstream)?;
        let local = self.local.as_str();
        let Some(from) = self.resolve_local(dir)? else {
            // deleted since classifying
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        if from == to {
            return Ok(UpdateDone::AlreadyThere);
        }
        // the fetch would write through it, unchecked
        if self.is_symref(dir)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        let refspec = format!("{to}:{local}");
        let mut args = LOCAL_FETCH_ARGS.to_vec();
        args.extend([".", refspec.as_str()]);
        let opts = CallOptions {
            allow_protocol: Some("file"),
            ..self.opts
        };
        let out = self.run(dir, &args, opts)?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // `<flag> <old> <new> <ref>`, one per ref it considered, the flag a
        // single char (a space for a fast-forward)
        let line = stdout.lines().find_map(|l| {
            let (flag, rest) = (l.get(..1)?, l.get(1..)?.strip_prefix(' ')?);
            let mut f = rest.splitn(3, ' ');
            let (old, new, r) = (f.next()?, f.next()?, f.next()?);
            (r == local).then_some((flag, old, new))
        });
        if !out.status.success() {
            return Err(match line {
                Some(("!", ..)) => format!("git rejected moving {local}: not a fast-forward"),
                _ => first_message(&out.stderr),
            });
        }
        // what git says it did, checked against the ref itself
        let Some(now) = self.resolve_local(dir)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        match line {
            Some((" ", old, new)) if new == to && now == to => Ok(UpdateDone::Updated {
                from: old.to_owned(),
                to,
            }),
            // up to date: moved there meanwhile
            None if now == to => Ok(UpdateDone::AlreadyThere),
            _ => Err(format!(
                "the fetch left {local} at {now}, not {to}: {}",
                stdout.trim()
            )),
        }
    }

    /// Fast-forwards the branch in `checkout`, the one it's on, to
    /// `upstream`'s tip, when it's still there and clean.
    pub(super) fn ff_in_checkout(
        &self,
        checkout: &Path,
        upstream: &str,
    ) -> Result<UpdateDone, String> {
        if let Some(by) = self.checkout_changed(checkout)? {
            return Ok(UpdateDone::Held(by));
        }
        let Some(from) = self.resolve_local(checkout)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        let to = self.resolve(checkout, upstream)?;
        if from == to {
            return Ok(UpdateDone::AlreadyThere);
        }
        if self.lazy_origin_moved(checkout)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        self.merge_ff(checkout, from, to)
    }

    /// `merge --ff-only` of `to` in `checkout`, then the check that it moved
    /// the branch from `from`: the merge moves whatever HEAD is on when it
    /// runs, and a HEAD switched since the status read it fails the action.
    /// A branch another hand moved past `to` meanwhile (git's "Already up
    /// to date") is held, not failed: nothing of it is lost.
    pub(super) fn merge_ff(
        &self,
        checkout: &Path,
        from: String,
        to: String,
    ) -> Result<UpdateDone, String> {
        self.run_checkout(
            checkout,
            &[
                "-c",
                "submodule.recurse=false",
                "merge",
                "--ff-only",
                "--no-overwrite-ignore",
                "--no-autostash",
                "--no-squash",
                "--no-stat",
                "--quiet",
                &to,
            ],
        )?;
        let local = self.local.as_str();
        let Some(now) = self.resolve_local(checkout)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        if now == to {
            return Ok(UpdateDone::Updated { from, to });
        }
        let head = self
            .git
            .output_string(checkout, &["symbolic-ref", "-q", "HEAD"], self.opts)
            .map_or_else(|_| "a detached HEAD".to_owned(), |s| s.trim().to_owned());
        if head == local && self.is_ancestor(checkout, &to, &now)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        Err(format!(
            "HEAD was on {head} when the merge ran; {local} is at {now}, not {to}"
        ))
    }

    /// Moves a shallow branch no checkout has to `upstream`'s tip, when it
    /// still has nothing on no remote: a compare-and-swap on the commit
    /// counted. A checkout switching to the branch in the instant between
    /// the re-checks and the update finds its HEAD moved under its files: a
    /// staged reverse diff, nothing lost.
    pub(super) fn move_in_place(&self, dir: &Path, upstream: &str) -> Result<UpdateDone, String> {
        let local = self.local.as_str();
        let Some(from) = self.resolve_local(dir)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        let to = self.resolve(dir, upstream)?;
        if from == to {
            return Ok(UpdateDone::AlreadyThere);
        }
        if self.has_local_work(dir, &from)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        // checked out nowhere, as git records it now
        let at = self.output_string(
            dir,
            &["for-each-ref", "--format=%(worktreepath)", local],
            self.opts,
        )?;
        if !at.trim().is_empty() || self.is_symref(dir)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        self.run_ok(
            dir,
            &[
                "update-ref",
                "--no-deref",
                "-m",
                "repos sync: move to the fetched tip",
                local,
                &to,
                &from,
            ],
            self.opts,
        )?;
        Ok(UpdateDone::Updated { from, to })
    }

    /// Moves a shallow branch in `checkout`, the one it's on, to
    /// `upstream`'s tip, when it's still there and clean and has nothing on
    /// no remote.
    pub(super) fn move_in_checkout(
        &self,
        checkout: &Path,
        upstream: &str,
    ) -> Result<UpdateDone, String> {
        if let Some(by) = self.checkout_changed(checkout)? {
            return Ok(UpdateDone::Held(by));
        }
        let Some(from) = self.resolve_local(checkout)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        let to = self.resolve(checkout, upstream)?;
        if from == to {
            return Ok(UpdateDone::AlreadyThere);
        }
        if self.has_local_work(checkout, &from)? || self.lazy_origin_moved(checkout)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        self.switch_reset(checkout, from, to)
    }

    /// Whether the lazy fetch a checkout would make may no longer reach
    /// the registry's repo over the transport decided, or alone: origin's
    /// URL, read again as git resolves it (`insteadOf` applied) — the URL
    /// the fetch connects to — names another repo, or another transport; or
    /// another promisor remote is configured now, which git would ask too.
    /// `false` with no lazy fetch: nothing reaches a remote.
    fn lazy_origin_moved(&self, dir: &Path) -> Result<bool, String> {
        let Some(lazy) = self.lazy else {
            return Ok(false);
        };
        let url = read_fetch_url(self.git, dir, self.opts).map_err(|e| git_message(&e))?;
        if !origin_matches(&url, lazy.repo) || lazy_transport(&url) != Some(lazy.transport) {
            return Ok(true);
        }
        let config = read_config(self.git, dir, self.opts, |_| false).map_err(|e| match e {
            ConfigReadError::Git(e) => git_message(&e),
            ConfigReadError::Exit { stderr } => first_message(&stderr),
            ConfigReadError::Parse(message) => message,
        })?;
        Ok(config.other_promisor)
    }

    /// `switch -C` to `to` in `checkout` — a shallow branch's fetched tip, or
    /// a rebased one's replayed tip — then the check that the branch it
    /// reset was at `from`, the commit counted or replayed: the switch is no
    /// compare-and-swap, so a commit made in between is dropped from the
    /// branch, and the reflog — written whatever the config says — is where
    /// it's found. A branch another hand moved to exactly `to` meanwhile
    /// gets no reflog entry from the switch (it moved nothing), so it's held:
    /// the move wasn't sync's.
    pub(super) fn switch_reset(
        &self,
        checkout: &Path,
        from: String,
        to: String,
    ) -> Result<UpdateDone, String> {
        self.run_checkout(
            checkout,
            &[
                "-c",
                "submodule.recurse=false",
                "-c",
                "core.logAllRefUpdates=true",
                "switch",
                "--no-overwrite-ignore",
                "--no-guess",
                "--quiet",
                "-C",
                self.branch,
                &to,
            ],
        )?;
        let local = self.local.as_str();
        let Some(now) = self.resolve_local(checkout)? else {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        };
        if now != to {
            return Err(format!(
                "{local} moved again after the switch to {to}: it's at {now}"
            ));
        }
        // the switch's own entry, `branch: Reset to <to>`, or none: it was
        // already there
        let newest = self.output_string(
            checkout,
            &[
                "log",
                "-g",
                "-1",
                "--no-show-signature",
                "--format=%gs",
                "--end-of-options",
                local,
                "--",
            ],
            self.opts,
        )?;
        if newest.trim() != format!("branch: Reset to {to}") {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        let before = self.resolve(checkout, &format!("{local}@{{1}}"))?;
        if before == from {
            Ok(UpdateDone::Updated { from, to })
        } else {
            Err(format!(
                "{local} was at {before}, not {from}, when the switch moved it to {to}: \
                 {before} is in its reflog (git reflog {local})"
            ))
        }
    }

    /// Why the checkout can't take the action now: its HEAD left the branch
    /// (`Changed`), or it has uncommitted changes (`DirtyCheckout`) — read
    /// fresh, as `status` reads a checkout. A branch made a symbolic ref
    /// reads as its target, so as `Changed`.
    pub(super) fn checkout_changed(
        &self,
        checkout: &Path,
    ) -> Result<Option<BranchSyncHold>, String> {
        let out = self.output(checkout, &STATUS_ARGS, self.opts)?;
        let status = porcelain::parse_status(&out)?;
        Ok(
            if !matches!(&status.head, Head::Branch { name } if name == self.branch) {
                Some(BranchSyncHold::Changed)
            } else if !status.uncommitted.is_clean() {
                Some(BranchSyncHold::DirtyCheckout)
            } else {
                None
            },
        )
    }

    /// Whether `commit` has commits on no remote-tracking ref, a shallow
    /// root aside — fetched, not made here — as the probe counts them.
    fn has_local_work(&self, dir: &Path, commit: &str) -> Result<bool, String> {
        Ok(self.count_local_work(dir, commit)? > 0)
    }

    /// `commit`'s commits on no remote-tracking ref, shallow roots aside.
    pub(super) fn count_local_work(&self, dir: &Path, commit: &str) -> Result<usize, String> {
        let out =
            self.output_string(dir, &["rev-list", commit, "--not", "--remotes"], self.opts)?;
        let roots = read_shallow_roots(self.common_dir);
        Ok(out.lines().filter(|c| !roots.contains(*c)).count())
    }

    /// Pushes `p.oid` to `p.target` on the registry's repo under a lease on
    /// the fetched tip, once the branch reads as classified — the same
    /// commit, upstream, and target, no symbolic ref — origin's push URL
    /// still names the registry's repo over SSH, and the commit is still the
    /// counted commits ahead of the remote-tracking ref, which is an
    /// ancestor of it. Then the remote-tracking ref moves to it
    /// (`record_push`). The module doc says what the lease and the ancestor
    /// check make of the push.
    pub(super) fn push(&self, dir: &Path, p: &Push<'_>) -> Result<PushDone, String> {
        let held = |by| Ok(PushDone::Stopped(Stop::Held(by)));
        if !self.reads_as_classified(dir, p)? {
            return held(BranchSyncHold::Changed);
        }
        // origin's push going elsewhere is a person's to sort out; the push
        // itself never reads it
        if !push_urls_match(&read_push_urls(self.git, dir, self.opts)?, p.url) {
            return held(BranchSyncHold::PushUrl);
        }
        let fetched = self.resolve(dir, p.upstream)?;
        if fetched == p.oid {
            return Ok(PushDone::AlreadyThere);
        }
        let ahead = if p.shallow {
            self.count_local_work(dir, p.oid)?
        } else {
            let range = format!("{fetched}..{}", p.oid);
            self.output_string(dir, &["rev-list", "--count", &range], self.opts)?
                .trim()
                .parse()
                .map_err(|_| format!("rev-list --count {range}: not a count"))?
        };
        // the lease lifts git's fast-forward check: this is it
        if !self.is_ancestor(dir, &fetched, p.oid)? || ahead != p.commits as usize {
            return held(BranchSyncHold::Changed);
        }
        let sent = self.send_pack(
            dir,
            &SendPack {
                oid: p.oid,
                target: p.target,
                expect: Some(&fetched),
                url: p.url,
                batch_ssh: p.batch_ssh,
            },
        )?;
        match sent {
            Sent::Pushed => {
                self.record_push(dir, p.upstream, &fetched, p.oid);
                // the lease held the remote at the fetched tip
                Ok(PushDone::Pushed {
                    from: fetched,
                    to: p.oid.to_owned(),
                })
            }
            // the remote holds the commit: the lease would have refused
            // that, so another hand pushed it in the instant between — and
            // the fetched tip was an ancestor of it, so the ref moves as if
            // this push had put it there
            Sent::UpToDate => {
                self.record_push(dir, p.upstream, &fetched, p.oid);
                Ok(PushDone::AlreadyThere)
            }
            Sent::Stopped(stop) => Ok(PushDone::Stopped(stop)),
        }
    }

    /// Creates the branch on the registry's repo — `refs/heads/<b>`, the
    /// same name — at `n.oid`, under a lease that it doesn't exist there,
    /// then sets its upstream as `git push -u` does: the remote-tracking ref
    /// its fetch refspec maps it to, created by compare-and-swap on none,
    /// then, when it had no upstream, `branch.<b>.remote` and `.merge`.
    ///
    /// First, as a push re-checks: the branch reads as classified — the
    /// same commit, a plain ref, and still no upstream configured (or its
    /// same-named upstream on origin, gone) — and origin's push URL still
    /// names the registry's repo over SSH. Then the remote-tracking ref it
    /// would track: none that origin's fetch refspec maps it to holds it
    /// for a person (`Unmapped`), and one there already, at another commit,
    /// is a branch the fetch found on origin, never adopted (`Exists`).
    ///
    /// The lease is a compare-and-swap on nothing: a branch created on the
    /// remote since the fetch fails it (`changed`), never overwritten, and
    /// one there at this very commit (another hand's, or this tool's own
    /// before its upstream was set) reads up to date, and the upstream is
    /// set all the same — so a run stopped between the push and the
    /// upstream is finished by the next.
    pub(super) fn create(&self, dir: &Path, n: &NewBranch<'_>) -> Result<Creation, String> {
        let held = |by| Ok(Creation::Stopped(Stop::Held(by)));
        let target = self.local.as_str();
        let unset = matches!(n.upstream, NewBranchUpstream::Unset);
        let upstream = match n.upstream {
            NewBranchUpstream::Unset => None,
            NewBranchUpstream::Gone { tracking } => Some([tracking, "origin", target]),
        };
        if !self.reads_as(dir, n.oid, upstream)? {
            return held(BranchSyncHold::Changed);
        }
        if unset && self.merge_configured(dir)? {
            return held(BranchSyncHold::Changed);
        }
        if !push_urls_match(&read_push_urls(self.git, dir, self.opts)?, n.url) {
            return held(BranchSyncHold::PushUrl);
        }
        let tracking = match n.upstream {
            NewBranchUpstream::Unset => match self.mapped_upstream(dir)? {
                Some(tracking) => tracking,
                None => return Ok(Creation::Unmapped),
            },
            NewBranchUpstream::Gone { tracking } => tracking.to_owned(),
        };
        let tracked = self.resolve_ref(dir, &tracking)?;
        match &tracked {
            Some(at) if at != n.oid => return Ok(Creation::Exists(at.clone())),
            _ => {}
        }
        let sent = self.send_pack(
            dir,
            &SendPack {
                oid: n.oid,
                target,
                expect: None,
                url: n.url,
                batch_ssh: n.batch_ssh,
            },
        )?;
        if let Sent::Stopped(stop) = sent {
            return Ok(Creation::Stopped(stop));
        }
        // as `git push -u`: the remote-tracking ref, then the upstream. The
        // ref is best effort, as a push's: a fetch that wrote it meanwhile
        // wrote what origin holds, and the next fetch writes it anyway
        // (until then, local refs read the upstream gone)
        if tracked.is_none() {
            self.update_tracking(dir, &tracking, n.oid, None);
        }
        if unset {
            self.set_upstream(dir).map_err(|e| {
                format!(
                    "{target} is on origin at {}, but setting {}'s upstream failed: {e} — \
                     rerun repos push --new-branch to set it",
                    n.oid, self.branch
                )
            })?;
        }
        Ok(Creation::Created(n.oid.to_owned()))
    }

    /// Sends `s.oid` to `s.target` on the registry's repo, over SSH, under a
    /// lease that the remote's ref is `s.expect` (`None`: that it doesn't
    /// exist, an empty value to git), and reads what git and the remote
    /// made of it.
    fn send_pack(&self, dir: &Path, s: &SendPack<'_>) -> Result<Sent, String> {
        let lease = format!(
            "--force-with-lease={}:{}",
            s.target,
            s.expect.unwrap_or_default()
        );
        let url = s.url.ssh();
        let refspec = format!("{}:{}", s.oid, s.target);
        let mut args = SEND_PACK_ARGS.to_vec();
        args.extend([lease.as_str(), url.as_str(), refspec.as_str()]);
        let opts = CallOptions {
            network: Some(NetworkOptions {
                batch_ssh: s.batch_ssh,
            }),
            // the registry's URL is SSH: nothing else may carry the push
            allow_protocol: Some("ssh"),
            ..self.opts
        };
        let out = match self.git.run(dir, &args, opts) {
            Ok(out) => out,
            Err(e) => {
                return Ok(Sent::Stopped(Stop::PushFailed(
                    RemoteFailure::from_git_error(e, RefspecContext::default()),
                )));
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        match pushed_ref(&stdout, s.target) {
            Some(PushedRef {
                ok: true,
                message: None,
            }) => Ok(Sent::Pushed),
            Some(PushedRef {
                ok: true,
                message: Some("up to date"),
            }) => Ok(Sent::UpToDate),
            Some(PushedRef {
                ok: true,
                message: Some(message),
            }) => Err(format!("git pushed {}: {message}", s.target)),
            Some(PushedRef { ok: false, message }) => Ok(Sent::Stopped(rejected(
                message.unwrap_or_default(),
                &out.stderr,
            ))),
            None if out.status.success() => {
                Err(format!("git send-pack reported nothing for {}", s.target))
            }
            None => Ok(Sent::Stopped(Stop::PushFailed(RemoteFailure::from_exit(
                out.status.code(),
                &out.stderr,
                RefspecContext::default(),
            )))),
        }
    }

    /// The commit `r` holds in `dir`, or `None` when there's no such ref.
    fn resolve_ref(&self, dir: &Path, r: &str) -> Result<Option<String>, String> {
        // `--quiet`: 1, silently, for a ref that doesn't exist
        let answer = self.answer(
            dir,
            &["rev-parse", "--verify", "--quiet", "--end-of-options", r],
        )?;
        Ok(answer.map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }

    /// Whether `branch.<b>.merge` is set anywhere git reads config: the
    /// branch has an upstream (or half of one) someone configured.
    fn merge_configured(&self, dir: &Path) -> Result<bool, String> {
        let key = format!("branch.{}.merge", self.branch);
        // 1 for a key that isn't set
        let answer = self.answer(dir, &["config", "--get-all", &key])?;
        Ok(answer.is_some())
    }

    /// The remote-tracking ref the branch would track as `origin`'s
    /// `refs/heads/<b>` — what git resolves as its upstream with that
    /// configured, through origin's fetch refspec — or `None` when the
    /// refspec maps it nowhere under `refs/remotes/origin/`.
    ///
    /// Read with the config passed for this call alone as `--config-env`,
    /// which git splits at the last `=` and whose values come whole from
    /// the environment: a branch named `a=b` reads as itself, where `-c`
    /// would split it at its first `=`.
    fn mapped_upstream(&self, dir: &Path) -> Result<Option<String>, String> {
        let name = self.branch;
        let local = self.local.as_str();
        let remote = format!("--config-env=branch.{name}.remote={UPSTREAM_REMOTE_VAR}");
        let merge = format!("--config-env=branch.{name}.merge={UPSTREAM_MERGE_VAR}");
        let env = [(UPSTREAM_REMOTE_VAR, "origin"), (UPSTREAM_MERGE_VAR, local)];
        let opts = CallOptions {
            env: &env,
            ..self.opts
        };
        let out = self.output_string(
            dir,
            &[
                &remote,
                &merge,
                "for-each-ref",
                "--format=%(refname)%00%(upstream)",
                local,
            ],
            opts,
        )?;
        // the pattern also matches refs under it (`<b>/x`): only the ref
        let upstream = out
            .lines()
            .filter_map(|l| l.split_once('\0'))
            .find(|(r, _)| *r == local)
            .map(|(_, upstream)| upstream);
        Ok(upstream
            .filter(|u| u.starts_with("refs/remotes/origin/"))
            .map(str::to_owned))
    }

    /// Sets the branch's upstream to origin's same-named branch, as `git
    /// push -u` does: `branch.<b>.remote`, then `.merge` — in that order, so
    /// a run stopped between leaves the merge unset, which still reads as
    /// no upstream (and the next `--new-branch` finishes it).
    fn set_upstream(&self, dir: &Path) -> Result<(), String> {
        for (key, value) in [("remote", "origin"), ("merge", self.local.as_str())] {
            let key = format!("branch.{}.{key}", self.branch);
            self.run_ok(dir, &["config", "--replace-all", &key, value], self.opts)?;
        }
        Ok(())
    }

    /// Moves `upstream`, the branch's remote-tracking ref, to `pushed`, as
    /// `git push` records a push, by compare-and-swap on `fetched`: a fetch
    /// that moved it in the meantime wrote what origin holds, and stands.
    /// Only a ref under `refs/remotes/origin/`, which classify's push
    /// verdict implies. Best effort (`update_tracking`).
    pub(super) fn record_push(&self, dir: &Path, upstream: &str, fetched: &str, pushed: &str) {
        self.update_tracking(dir, upstream, pushed, Some(fetched));
    }

    /// Moves `tracking`, a remote-tracking ref, to `new` by compare-and-swap
    /// on `old` (`None`: that it doesn't exist, an empty value to git), as
    /// `git push` records a push. Only a ref under `refs/remotes/origin/`.
    ///
    /// Best effort, whatever git makes of it: the push it records stands
    /// whatever the ref says, and the next fetch writes what origin holds.
    fn update_tracking(&self, dir: &Path, tracking: &str, new: &str, old: Option<&str>) {
        if !tracking.starts_with("refs/remotes/origin/") {
            return;
        }
        let _ = self.git.run(
            dir,
            &[
                "update-ref",
                "--no-deref",
                "-m",
                "repos: update by push",
                tracking,
                new,
                old.unwrap_or_default(),
            ],
            self.opts,
        );
    }

    /// Whether the branch in `dir` is as classified: the commit `p.oid`, a
    /// plain ref, its upstream the same remote-tracking ref, on `origin`,
    /// at `p.target` there. `false` when deleted since.
    fn reads_as_classified(&self, dir: &Path, p: &Push<'_>) -> Result<bool, String> {
        self.reads_as(dir, p.oid, Some([p.upstream, "origin", p.target]))
    }

    /// Whether the branch in `dir` holds `oid`, is a plain ref, and has
    /// `upstream` — its resolved remote-tracking ref, the remote, and the
    /// ref there — or none, `None`. `false` when deleted since.
    pub(super) fn reads_as(
        &self,
        dir: &Path,
        oid: &str,
        upstream: Option<[&str; 3]>,
    ) -> Result<bool, String> {
        let local = self.local.as_str();
        let out = self.output_string(
            dir,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(symref)%00%(upstream)%00\
                 %(upstream:remotename)%00%(upstream:remoteref)",
                local,
            ],
            self.opts,
        )?;
        // git prints an empty field for each part of an upstream there isn't,
        // as `%(symref)` is empty for a plain ref
        let [tracking, remote, remote_ref] = upstream.unwrap_or_default();
        let expected = [local, oid, "", tracking, remote, remote_ref];
        // the pattern also matches refs under it (`<b>/x`): only the ref
        Ok(out
            .lines()
            .map(|l| l.split('\0').collect::<Vec<_>>())
            .find(|f| f.first() == Some(&local))
            .is_some_and(|f| f == expected))
    }

    /// Runs git in `checkout` to rewrite its working tree, under
    /// `CHECKOUT_TIMEOUT` — in a partial clone, with its lazy fetch
    /// (`LazyFetch`): the new tip's blobs in the checkout's cone may never
    /// have been fetched, and git reads them from the promisor remote.
    fn run_checkout(&self, checkout: &Path, args: &[&str]) -> Result<(), String> {
        let mut opts = CallOptions {
            timeout: Some(CHECKOUT_TIMEOUT),
            ..self.opts
        };
        if let Some(lazy) = self.lazy {
            opts.lazy_fetch = true;
            opts.allow_protocol = Some(lazy.transport);
            opts.network = Some(NetworkOptions {
                batch_ssh: lazy.batch_ssh,
            });
        }
        self.run_ok(checkout, args, opts)
    }

    /// Runs git in `dir`, failing with git's message unless it succeeds.
    pub(super) fn run_ok(
        &self,
        dir: &Path,
        args: &[&str],
        opts: CallOptions<'_>,
    ) -> Result<(), String> {
        let out = self.run(dir, args, opts)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(first_message(&out.stderr))
        }
    }
}

/// One ref's line in `git send-pack --helper-status`'s output: `ok <ref>`,
/// `ok <ref> up to date`, or `error <ref> <why>` — git's reason (`stale
/// info`; `no match` for each remote ref not pushed) or the remote's own.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct PushedRef<'a> {
    pub(super) ok: bool,
    /// What follows the ref, its surrounding quotes dropped (git C-quotes a
    /// message holding special characters).
    pub(super) message: Option<&'a str>,
}

/// The status line for the push to `dst`, if git printed one.
pub(super) fn pushed_ref<'a>(stdout: &'a str, dst: &str) -> Option<PushedRef<'a>> {
    stdout.lines().find_map(|l| {
        let (status, rest) = l.split_once(' ')?;
        let ok = match status {
            "ok" => true,
            "error" => false,
            _ => return None,
        };
        let (r, message) = rest
            .split_once(' ')
            .map_or((rest, None), |(r, m)| (r, Some(m)));
        let unquoted = |m: &'a str| {
            m.strip_prefix('"')
                .and_then(|m| m.strip_suffix('"'))
                .unwrap_or(m)
        };
        (r == dst).then(|| PushedRef {
            ok,
            message: message.map(unquoted),
        })
    })
}

/// A push git or the remote refused, by why: the remote's branch isn't the
/// fetched tip (`stale info`, the lease's refusal — moved or deleted since
/// the fetch; `fetch first` and `non-fast forward`, git's own, should a
/// lease ever not apply) is held, `Changed`, for a rerun to reclassify;
/// git's other refusals fail with their words; anything else is the
/// remote's refusal (a ruleset, a hook), failed with its reason and the
/// remote's first error line. A remote whose reason reads exactly as one
/// of git's is taken for git's: nothing was pushed either way.
pub(super) fn rejected(why: &str, stderr: &str) -> Stop {
    match why {
        "stale info" | "fetch first" | "non-fast forward" => Stop::Held(BranchSyncHold::Changed),
        "needs force"
        | "already exists"
        | "remote ref updated since checkout"
        | "no match"
        | "expecting report"
        | "atomic push failed"
        | "" => Stop::PushFailed(RemoteFailure::Failed {
            message: format!(
                "rejected: {}",
                if why.is_empty() { "no reason" } else { why }
            ),
        }),
        reason => Stop::PushFailed(RemoteFailure::Rejected {
            reason: reason.to_owned(),
            message: remote_error(stderr),
        }),
    }
}

/// The remote's first `error:` line, as git relays it (`remote: error: …`,
/// padded), else its first line; `None` when it sent none.
fn remote_error(stderr: &str) -> Option<String> {
    let remote = || {
        stderr
            .lines()
            .filter_map(|l| l.strip_prefix("remote:"))
            .map(str::trim)
            .filter(|l| !l.is_empty())
    };
    remote()
        .find_map(|l| l.strip_prefix("error:").map(str::trim))
        .or_else(|| remote().next())
        .map(str::to_owned)
}

/// A git call's failure as an outcome's message: git's own words when it
/// ran and refused.
fn git_message(e: &GitError) -> String {
    match e {
        GitError::Failed { stderr, .. } => first_message(stderr),
        e => e.to_string(),
    }
}

/// The most paths `first_message` carries from a list git prints under its
/// message; the rest are counted.
const MAX_LISTED_PATHS: usize = 5;

/// Git's first `fatal:` or `error:` line, else its first line, else a
/// placeholder for a git that said nothing.
///
/// A line ending in a colon heads a list — the files a checkout or merge
/// would overwrite, each on its own tab-indented line — and the line alone
/// names none of them: the paths listed right under it follow it, the
/// first `MAX_LISTED_PATHS` and a count of the rest.
pub(super) fn first_message(stderr: &str) -> String {
    let lines = || stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines()
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .or_else(|| lines().next())
    else {
        return "git failed without a message".to_owned();
    };
    if !first.ends_with(':') {
        return first.to_owned();
    }
    let listed: Vec<&str> = stderr
        .lines()
        .skip_while(|l| l.trim() != first)
        .skip(1)
        .take_while(|l| l.starts_with('\t'))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if listed.is_empty() {
        return first.to_owned();
    }
    let shown = listed[..listed.len().min(MAX_LISTED_PATHS)].join(", ");
    match listed.len() - listed.len().min(MAX_LISTED_PATHS) {
        0 => format!("{first} {shown}"),
        more => format!("{first} {shown}, and {more} more"),
    }
}
