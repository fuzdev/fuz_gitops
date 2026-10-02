//! A rebase's git calls (`Step::rebase`): the replay of a diverged branch's
//! local-only commits onto the fetched tip, and the move of the branch to
//! what it made. What each relies on, and the races it leaves, is the
//! `sync` module's doc; the push that follows is `step`'s.

use std::path::Path;

use super::step::{CHECKOUT_TIMEOUT, Step, UpdateDone, first_message};
use crate::git::CallOptions;
use crate::probe::TAGGED_ARGS;
use crate::report::{BranchSyncHold, RebaseRefusal};

/// The variable `Step::merge_driver_overrides`'s `--config-env` reads each
/// configured merge driver's replacement from: `false`, a command that
/// fails.
const MERGE_DRIVER_VAR: &str = "REPOS_MERGE_DRIVER";

/// The replay's config, before each configured merge driver's override and
/// the command itself.
///
/// - `rerere.enabled=false`: no recorded resolution is applied.
/// - `merge.default=text`: a path with no `merge` attribute takes git's
///   three-way text merge, whatever driver the config makes the default.
/// - `merge.union.driver=false`: a path whose attributes ask for the
///   built-in `union` merge — which takes both sides' lines and reports no
///   conflict — conflicts instead: a driver by that name defined here
///   stands in for the built-in, and its failure is a conflict.
/// - `user.useConfigOnly=true`: the replayed commits' committer is an
///   identity the user set (`user.name` and `user.email`, or the
///   environment's), never git's guess from the login and host name, which
///   the push would publish.
/// - `replay.refAction=print`: the config form of `--ref-action=print`,
///   for a git that reads it; this crate's oldest git ignores the key.
///   Whether any git reads it isn't verified here: the flag
///   (`replay_takes_ref_action`) and the re-read of the branch after are
///   what the action relies on.
const REPLAY_ARGS: [&str; 10] = [
    "-c",
    "rerere.enabled=false",
    "-c",
    "merge.default=text",
    "-c",
    "merge.union.driver=false",
    "-c",
    "user.useConfigOnly=true",
    "-c",
    "replay.refAction=print",
];

/// A failed replay's message: git's, and for an identity it won't guess,
/// what to set.
fn replay_failure(stderr: &str) -> String {
    let message = first_message(stderr);
    if message.contains("auto-detection is disabled") {
        format!(
            "{message} — a rebase commits as you: set user.name and user.email (git config \
             --global user.name …, git config --global user.email …), then rerun"
        )
    } else {
        message
    }
}

/// What a rebase replays, as classified (`Step::rebase`).
pub(super) struct Rebase<'a> {
    /// The commit the branch held when probed: the tip of what's replayed.
    pub(super) oid: &'a str,
    /// The resolved upstream ref, under `refs/remotes/origin/`: its commit
    /// is what the local-only commits are replayed onto.
    pub(super) upstream: &'a str,
    /// The upstream's ref on origin (`push_target`): the branch's own name
    /// there.
    pub(super) target: &'a str,
    /// The commits ahead and behind the verdict counted.
    pub(super) ahead: u32,
    pub(super) behind: u32,
}

/// How a rebase went, short of failing (`Actor::rebase`).
#[derive(Debug)]
pub(super) enum RebaseDone {
    /// The branch moved from `from` to `to`, its local-only commits
    /// replayed onto `onto`, the fetched tip: ahead of it now, to push.
    Rebased {
        from: String,
        to: String,
        onto: String,
    },
    /// A re-check held it; nothing moved.
    Held(BranchSyncHold),
    /// The replay found it a person's; nothing moved.
    Refused(RebaseRefusal),
}

/// What `git replay` came to (`Step::replay`).
enum Replayed {
    /// The replayed tip: commits written, no ref moved.
    Tip(String),
    /// A conflict: nothing written that anything names.
    Conflicts,
    /// The branch didn't hold the commit classified when git read it, or
    /// doesn't after.
    Moved,
}

/// One commit of a chain (`Step::chain`).
struct Link {
    commit: String,
    tree: String,
    parents: Vec<String>,
}

impl Step<'_> {
    /// Replays the branch's local-only commits onto `r.upstream`'s tip and
    /// moves the branch to the replayed tip — in `checkout`, the one it's
    /// on, or in place when no checkout has it. The push that follows is
    /// the caller's (`Step::push`, of the tip this returns).
    ///
    /// First, as a push re-checks: the checkout still on the branch and
    /// clean, the branch as classified (the same commit, upstream, and ref
    /// on origin, a plain ref), and that commit still the counted commits
    /// ahead of and behind the remote-tracking ref, no merge among them.
    /// Then `git replay`, which rewrites no ref, index, or working tree: a
    /// conflict stops here with nothing moved (`RebaseRefusal::Conflicts`),
    /// as does a replayed commit that came out empty though its original
    /// wasn't (`AlreadyUpstream`). Then the move, by `replayed`. The module
    /// doc says what each step relies on and the windows left.
    pub(super) fn rebase(
        &self,
        dir: &Path,
        checkout: Option<&Path>,
        r: &Rebase<'_>,
    ) -> Result<RebaseDone, String> {
        let held = |by| Ok(RebaseDone::Held(by));
        if let Some(checkout) = checkout
            && let Some(by) = self.checkout_changed(checkout)?
        {
            return held(by);
        }
        if !self.reads_as(dir, r.oid, Some([r.upstream, "origin", r.target]))? {
            return held(BranchSyncHold::Changed);
        }
        let onto = self.resolve(dir, r.upstream)?;
        if !self.diverged_as_classified(dir, &onto, r)? {
            return held(BranchSyncHold::Changed);
        }
        let to = match self.replay(dir, &onto, r.oid)? {
            Replayed::Tip(to) => to,
            Replayed::Conflicts => return Ok(RebaseDone::Refused(RebaseRefusal::Conflicts)),
            Replayed::Moved => return held(BranchSyncHold::Changed),
        };
        if let Some(commit) = self.already_upstream(dir, &onto, r.oid, &to)? {
            return Ok(RebaseDone::Refused(RebaseRefusal::AlreadyUpstream {
                commit,
            }));
        }
        let from = r.oid.to_owned();
        let moved = match checkout {
            Some(checkout) => self.replayed_in_checkout(checkout, from, to)?,
            None => self.replayed_in_place(dir, from, to)?,
        };
        Ok(match moved {
            UpdateDone::Updated { from, to } => RebaseDone::Rebased { from, to, onto },
            // unreachable: the replayed tip is a commit made just now
            UpdateDone::AlreadyThere => RebaseDone::Held(BranchSyncHold::Changed),
            UpdateDone::Held(by) => RebaseDone::Held(by),
        })
    }

    /// Whether `r.oid` still stands against `onto`, the remote-tracking
    /// ref's commit now, as classified: the same commits ahead and behind,
    /// every one ahead on no remote-tracking ref, and no merge among them,
    /// nor a tag on one.
    fn diverged_as_classified(
        &self,
        dir: &Path,
        onto: &str,
        r: &Rebase<'_>,
    ) -> Result<bool, String> {
        let symmetric = format!("{onto}...{}", r.oid);
        let counts = self.output_string(
            dir,
            &["rev-list", "--count", "--left-right", &symmetric],
            self.opts,
        )?;
        // `<behind>\t<ahead>`: the left side is the upstream's
        let expected = format!("{}\t{}", r.behind, r.ahead);
        if counts.trim() != expected {
            return Ok(false);
        }
        // a commit ahead that another remote-tracking ref holds now — a
        // fetch, or a push by another hand, wrote one since — is never
        // rewritten: the commits on no remote are a subset of the ones
        // ahead, so the same count is the same commits
        if self.count_local_work(dir, r.oid)? != r.ahead as usize {
            return Ok(false);
        }
        // a second line behind `reads_as` and the counts: the same commit
        // against the same upstream commit holds the merges classify
        // counted, none, so this is reached only should those ever let a
        // changed range through
        let range = format!("{onto}..{}", r.oid);
        let merges =
            self.output_string(dir, &["rev-list", "--count", "--merges", &range], self.opts)?;
        if merges.trim() != "0" {
            return Ok(false);
        }
        let mut args = TAGGED_ARGS.to_vec();
        args.extend(["--merged", r.oid, "--no-merged", onto, "refs/tags"]);
        let tags = self.output_string(dir, &args, self.opts)?;
        Ok(tags.trim().is_empty())
    }

    /// `git replay` of `onto..<branch>` onto `onto`: the replayed tip, when
    /// the branch git read held `from`.
    ///
    /// The branch is named by its ref, since replay prints the ref update
    /// only for a ref named in the range (a range of bare commits prints
    /// nothing): `update <ref> <new> <old>`, and `<old>`, the commit it
    /// replayed from, must be `from`. Git that updates the refs itself
    /// unless told to print them is told to, both ways it might be told
    /// (`REPLAY_ARGS`, `replay_takes_ref_action`); either way the branch
    /// must still hold `from` after.
    ///
    /// Nothing settles a conflict (`REPLAY_ARGS`, `merge_driver_overrides`):
    /// `rerere` is off, and no merge driver runs — the user's, or git's
    /// `union` — so a path one would have merged is a conflict like any
    /// other. Attributes are read from `from`'s own tree (`--attr-source`),
    /// not the checkout the replay happens to run in, so a path the branch
    /// marks unmergeable conflicts wherever it's checked out, or nowhere.
    /// The committer is the identity the user configured, never one
    /// git derives from the login and host name: with none, git refuses and
    /// the action fails, saying what to set.
    fn replay(&self, dir: &Path, onto: &str, from: &str) -> Result<Replayed, String> {
        let local = self.local.as_str();
        let range = format!("{onto}..{local}");
        let drivers = self.merge_driver_overrides(dir)?;
        // attributes from the branch replayed, wherever the replay runs: the
        // primary checkout may be on another branch, whose `.gitattributes`
        // would miss a path this one marks unmergeable. `info/attributes`
        // and `core.attributesFile` still apply over it
        let attr_source = format!("--attr-source={from}");
        let mut args = vec![attr_source.as_str()];
        args.extend(REPLAY_ARGS);
        args.extend(drivers.iter().map(String::as_str));
        args.push("replay");
        if self.replay_takes_ref_action(dir)? {
            args.push("--ref-action=print");
        }
        args.extend(["--onto", onto, &range]);
        let env = [(MERGE_DRIVER_VAR, "false")];
        let opts = CallOptions {
            timeout: Some(CHECKOUT_TIMEOUT),
            env: &env,
            ..self.opts
        };
        let out = self.run(dir, &args, opts)?;
        match out.status.code() {
            Some(0) => {}
            // a conflict, and only that: git dies with 128 on anything else
            Some(1) if out.stdout.is_empty() => return Ok(Replayed::Conflicts),
            _ => return Err(replay_failure(&out.stderr)),
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            // nothing printed: a git that updated the ref itself, past both
            // requests to print — its checkout's files left at the old
            // commit — or one that printed nothing and moved nothing
            let now = self.resolve_local(dir)?;
            return Err(if now.as_deref() == Some(from) {
                format!("git replay printed no ref update for {local}")
            } else {
                format!(
                    "git replay moved {local} itself, from {from} to {}, where repos asked it \
                     to print the update: a checkout on the branch wasn't updated and reads \
                     as changed — inspect it by hand ({from} is the commit it held)",
                    now.as_deref().unwrap_or("no commit")
                )
            });
        }
        let mut updates = stdout.lines().map(|l| l.split(' ').collect::<Vec<_>>());
        let (Some(update), None) = (updates.next(), updates.next()) else {
            return Err(format!(
                "git replay printed no single ref update for {local}: {}",
                stdout.trim()
            ));
        };
        let ["update", r, new, old] = update[..] else {
            return Err(format!("git replay printed `{}`", stdout.trim()));
        };
        if r != local {
            return Err(format!("git replay updated {r}, not {local}"));
        }
        // the branch moved under the replay, or git moved it itself
        if old != from || self.resolve_local(dir)?.as_deref() != Some(from) {
            return Ok(Replayed::Moved);
        }
        Ok(Replayed::Tip(new.to_owned()))
    }

    /// Each merge driver the config defines (`merge.<name>.driver`), set for
    /// the replay alone to a command that fails — as `--config-env`
    /// arguments reading `MERGE_DRIVER_VAR`, since a driver's name may hold
    /// an `=` that `-c` would split at. Git takes a driver's failure for a
    /// conflict and never runs the program configured, so a path a driver
    /// would have merged stops the rebase, as `union` does (`REPLAY_ARGS`).
    fn merge_driver_overrides(&self, dir: &Path) -> Result<Vec<String>, String> {
        // 1 when no key matches
        let listed = self.answer(
            dir,
            &[
                "config",
                "-z",
                "--name-only",
                "--get-regexp",
                r"^merge\..*\.driver$",
            ],
        )?;
        let Some(out) = listed else {
            return Ok(Vec::new());
        };
        let keys = String::from_utf8_lossy(&out.stdout);
        Ok(keys
            .split('\0')
            .filter(|key| !key.is_empty())
            .map(|key| format!("--config-env={key}={MERGE_DRIVER_VAR}"))
            .collect())
    }

    /// Whether this git's `replay` takes `--ref-action`, from its usage: a
    /// git with the option updates the refs itself by default, which would
    /// move a checked-out branch under its files.
    fn replay_takes_ref_action(&self, dir: &Path) -> Result<bool, String> {
        // `-h`: the usage, and exit 129
        let out = self.run(dir, &["replay", "-h"], self.opts)?;
        let usage = String::from_utf8_lossy(&out.stdout);
        Ok(usage.contains("--ref-action") || out.stderr.contains("--ref-action"))
    }

    /// The first of the local-only commits (`onto..from`, oldest first)
    /// whose replay (`onto..to`) changes nothing though the original
    /// changed something: its change is in the upstream already, and
    /// replay kept it as an empty commit. `None` when none is.
    fn already_upstream(
        &self,
        dir: &Path,
        onto: &str,
        from: &str,
        to: &str,
    ) -> Result<Option<String>, String> {
        let originals = self.chain(dir, &format!("{onto}..{from}"))?;
        let replayed = self.chain(dir, &format!("{onto}..{to}"))?;
        if originals.len() != replayed.len() {
            return Err(format!(
                "git replay made {} commits of {}",
                replayed.len(),
                originals.len()
            ));
        }
        for (original, new) in originals.iter().zip(&replayed) {
            let [parent] = &new.parents[..] else {
                return Err(format!("git replay made {} no single parent", new.commit));
            };
            if new.tree != self.tree_of(dir, &replayed, parent)? {
                continue;
            }
            // a root commit changes something unless its tree is empty,
            // which no replay of it would be compared with
            let was_empty = match &original.parents[..] {
                [parent] => original.tree == self.tree_of(dir, &originals, parent)?,
                _ => false,
            };
            if !was_empty {
                return Ok(Some(original.commit.clone()));
            }
        }
        Ok(None)
    }

    /// The tree of `commit`: its link's when `chain` holds it, else as git
    /// reads it (the commit a chain starts from).
    fn tree_of(&self, dir: &Path, chain: &[Link], commit: &str) -> Result<String, String> {
        if let Some(link) = chain.iter().find(|l| l.commit == commit) {
            return Ok(link.tree.clone());
        }
        let tree = format!("{commit}^{{tree}}");
        self.output_string(
            dir,
            &["rev-parse", "--verify", "--end-of-options", &tree],
            self.opts,
        )
        .map(|s| s.trim().to_owned())
    }

    /// The commits of `range`, oldest first, each with its tree and
    /// parents.
    fn chain(&self, dir: &Path, range: &str) -> Result<Vec<Link>, String> {
        let out = self.output_string(
            dir,
            &[
                "rev-list",
                "--reverse",
                "--topo-order",
                "--no-commit-header",
                "--format=%H %T %P",
                "--end-of-options",
                range,
            ],
            self.opts,
        )?;
        out.lines()
            .map(|l| {
                let mut f = l.split(' ');
                let (Some(commit), Some(tree)) = (f.next(), f.next()) else {
                    return Err(format!("rev-list {range}: `{l}`"));
                };
                Ok(Link {
                    commit: commit.to_owned(),
                    tree: tree.to_owned(),
                    parents: f.filter(|p| !p.is_empty()).map(str::to_owned).collect(),
                })
            })
            .collect()
    }

    /// Moves a branch no checkout has from `from` to its replayed tip `to`:
    /// a compare-and-swap on `from`, the commit replayed, once it reads
    /// checked out nowhere and no symbolic ref. A branch another hand moved
    /// in between fails the swap and is held (`Changed`).
    fn replayed_in_place(
        &self,
        dir: &Path,
        from: String,
        to: String,
    ) -> Result<UpdateDone, String> {
        let local = self.local.as_str();
        let at = self.output_string(
            dir,
            &["for-each-ref", "--format=%(worktreepath)", local],
            self.opts,
        )?;
        if !at.trim().is_empty() || self.is_symref(dir)? {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        // the reflog is where the replaced commits stay reachable: written
        // whatever the config says, as the switch's is
        let swapped = self.run_ok(
            dir,
            &[
                "-c",
                "core.logAllRefUpdates=true",
                "update-ref",
                "--no-deref",
                "-m",
                "repos: rebase onto the fetched tip",
                local,
                &to,
                &from,
            ],
            self.opts,
        );
        match swapped {
            Ok(()) => Ok(UpdateDone::Updated { from, to }),
            Err(message) => {
                if self.resolve_local(dir)?.as_deref() == Some(from.as_str()) {
                    Err(message)
                } else {
                    Ok(UpdateDone::Held(BranchSyncHold::Changed))
                }
            }
        }
    }

    /// Moves the branch in `checkout`, the one it's on, from `from` to its
    /// replayed tip `to`, when it's still there, clean, and at `from` —
    /// read again, the replay done — with the switch a shallow move makes
    /// (`switch_reset`), and its check after.
    fn replayed_in_checkout(
        &self,
        checkout: &Path,
        from: String,
        to: String,
    ) -> Result<UpdateDone, String> {
        if let Some(by) = self.checkout_changed(checkout)? {
            return Ok(UpdateDone::Held(by));
        }
        if self.resolve_local(checkout)?.as_deref() != Some(from.as_str()) {
            return Ok(UpdateDone::Held(BranchSyncHold::Changed));
        }
        self.switch_reset(checkout, from, to)
    }
}
