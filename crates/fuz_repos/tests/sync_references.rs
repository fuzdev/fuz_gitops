//! Refreshing references over fixture workspaces: a third-party reference
//! is fetched (over the fixture's `https`) and acted on only when a run
//! asks — named as a target (`Refresh::Named`) or under `--references` —
//! and never pushed; a pin named is refused and never fetched, and so is a
//! refresh whose origin isn't the repo over HTTPS (drifted, SSH, or
//! rewritten by `insteadOf`), held instead; a partial
//! clone's checkout fetches the blobs it needs; and a missing entry already
//! cloned under an unregistered name is held. Each run is followed by the
//! exact refs, HEAD, and working tree it should leave, and by what reached
//! a remote (the fixture's `https` and `ssh` logs).

// helpers outside `#[test]` fns fail the test the way an assertion would
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use fuz_repos::classify::{NeedsHuman, OriginFix, Refresh};
use fuz_repos::report::{
    BranchOutcome, BranchSyncHold, CloneOutcome, CloneSyncHold, EntrySync, FetchOutcome,
};
use fuz_repos::sessions::LiveSessions;
use fuz_repos::state::{
    BranchNeedsHuman, CloneHold, CloneVerdict, Presence, ProbeErrorKind, RefreshHold,
    RefreshVerdict, Relation, SyncAction, Verdict,
};
use fuz_repos::sync::SyncRun;
use support::{FixtureWorkspace, OWNER, THIRD_PARTY, branch, find_entry, write};

const fn ff(commits: u32) -> SyncAction {
    SyncAction::FastForward { commits }
}

fn outcomes<'a>(run: &'a SyncRun, key: &str) -> &'a EntrySync {
    run.outcomes
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no outcomes for {key}: {:?}", run.outcomes))
}

fn outcome<'a>(run: &'a SyncRun, key: &str, name: &str) -> &'a BranchOutcome {
    &outcomes(run, key)
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch {key}:{name}: {:?}", run.outcomes))
        .outcome
}

fn https_url(name: &str) -> String {
    format!("https://github.com/{THIRD_PARTY}/{name}")
}

/// The files of a checkout, `.git` aside, relative and sorted.
fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// How many objects reachable from any ref the repo lacks: a partial
/// clone's unfetched blobs.
fn missing_objects(ws: &FixtureWorkspace, repo: &Path) -> usize {
    ws.git(repo, &["rev-list", "--objects", "--all", "--missing=print"])
        .lines()
        .filter(|l| l.starts_with('?'))
        .count()
}

/// The workspace root's entries, sorted: what a run left there.
fn root_listing(ws: &FixtureWorkspace) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(ws.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// A third-party reference `name`, cloned (with `args`) with `origin` its
/// HTTPS URL and nothing rewriting it, served by the fixture's `https`;
/// then upstream moves `main`, which the clone doesn't know yet. Returns
/// the clone and upstream's new tip.
fn reference_behind(ws: &mut FixtureWorkspace, name: &str, args: &[&str]) -> (PathBuf, String) {
    ws.remote(name, &[("a.txt", "a\n")]);
    ws.declare_reference(name, THIRD_PARTY, name, "");
    let dir = ws.clone_third_party_over_https(name, name, args);
    let tip = ws.upstream_commit(name, "main");
    ws.serve_https();
    ws.write_registry();
    ws.assert_head(&dir, Some("main"));
    ws.assert_upstream(&dir, "main", "refs/remotes/origin/main");
    ws.assert_track(&dir, "main", "");
    ws.assert_clean(&dir);
    assert_ne!(ws.git(&dir, &["rev-parse", "main"]), tip);
    (dir, tip)
}

// --- untouched unless asked ---

#[test]
fn a_third_party_reference_is_never_fetched_unless_asked() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    // local work, the only thing said of it by default
    ws.git(&lib, &["switch", "-q", "-c", "audit"]);
    ws.commit(&lib, "audit");
    ws.git(&lib, &["switch", "-q", "main"]);
    let before = ws.refs(&lib);

    let run = ws.sync();

    let s = outcomes(&run, "lib");
    assert_eq!(s.fetch, FetchOutcome::NotFetched);
    let e = find_entry(&run.entries, "lib");
    assert_eq!(e.refresh, None);
    // no branch compared against a remote never fetched: local work alone
    assert_eq!(e.branches.len(), 1, "{:?}", e.branches);
    assert_eq!(branch(e, "audit").relation, Relation::Untracked);
    assert_eq!(branch(e, "audit").verdict, Verdict::LocalOnly);
    assert_eq!(s.branches[0].outcome, BranchOutcome::Untouched);
    // `status --fetch` passes it over too
    let e = support::take_entry(ws.status_with_fetch(), "lib");
    assert_eq!((e.refresh, e.fetch_error), (None, None));
    // nothing reached for the remote, served or refused
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert!(ws.https_refused_log().is_empty());
    assert!(ws.ssh_log().is_empty());
    assert_eq!(ws.refs(&lib), before);
}

#[test]
fn a_named_reference_is_fetched_over_https_and_fast_forwarded() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[("a.txt", "a\n")]);
    ws.upstream_commit("lib", "feat");
    ws.declare_reference("lib", THIRD_PARTY, "lib", "");
    let lib = ws.clone_third_party_over_https("lib", "lib", &[]);
    // a second branch, checked out nowhere
    ws.git(&lib, &["branch", "-q", "--track", "feat", "origin/feat"]);
    let tip = ws.upstream_commit("lib", "main");
    let feat_tip = ws.upstream_commit("lib", "feat");
    ws.serve_https();
    ws.write_registry();
    ws.assert_head(&lib, Some("main"));
    ws.assert_track(&lib, "feat", "");
    ws.assert_clean(&lib);
    let before = ws.refs(&lib);
    assert_ne!(before["refs/heads/main"], tip);
    assert_ne!(before["refs/heads/feat"], feat_tip);

    let run = ws.sync_asked(Refresh::Named, 4);

    let e = find_entry(&run.entries, "lib");
    assert_eq!(e.refresh, Some(RefreshVerdict::Act));
    assert_eq!(branch(e, "main").relation, Relation::Behind { commits: 1 });
    assert_eq!(branch(e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(outcomes(&run, "lib").fetch, FetchOutcome::Fetched);
    assert_eq!(
        outcome(&run, "lib", "main"),
        &BranchOutcome::FastForwarded {
            from: before["refs/heads/main"].clone(),
            to: tip.clone(),
        }
    );
    assert_eq!(
        outcome(&run, "lib", "feat"),
        &BranchOutcome::FastForwarded {
            from: before["refs/heads/feat"].clone(),
            to: feat_tip.clone(),
        }
    );
    // over HTTPS alone, once
    assert_eq!(ws.https_log(), [https_url("lib")]);
    assert!(ws.ssh_log().is_empty());
    assert_eq!(
        ws.refs(&lib),
        ws.refs_after_fetch(
            "lib",
            &before,
            &[("refs/heads/main", &tip), ("refs/heads/feat", &feat_tip)]
        )
    );
    ws.assert_head(&lib, Some("main"));
    ws.assert_clean(&lib);
    assert!(lib.join("upstream-main.txt").is_file());
    // no tags, as every fetch of the tool's
    assert!(ws.git(&lib, &["tag"]).is_empty());
}

#[test]
fn references_refreshes_every_third_party_reference_and_no_pin() {
    let mut ws = FixtureWorkspace::new();
    let (lib, lib_tip) = reference_behind(&mut ws, "lib", &[]);
    let (dom, dom_tip) = reference_behind(&mut ws, "dom", &[]);
    // a third-party pin and an owned one: named by no one, left alone
    ws.remote("oracle", &[]);
    ws.declare_reference("oracle", THIRD_PARTY, "oracle", "pinned = true");
    let oracle = ws.clone_third_party_over_https("oracle", "oracle", &[]);
    ws.upstream_commit("oracle", "main");
    ws.remote("wpt", &[]);
    ws.declare_reference("wpt", OWNER, "wpt", "pinned = true");
    let wpt = ws.clone_owned("wpt", "wpt", &[]);
    ws.upstream_commit("wpt", "main");
    ws.write_registry();
    let (lib_before, dom_before) = (ws.refs(&lib), ws.refs(&dom));
    let (oracle_before, wpt_before) = (ws.refs(&oracle), ws.refs(&wpt));

    let run = ws.sync_asked(Refresh::References, 4);

    for (key, dir, before, tip) in [
        ("lib", &lib, &lib_before, &lib_tip),
        ("dom", &dom, &dom_before, &dom_tip),
    ] {
        assert_eq!(
            find_entry(&run.entries, key).refresh,
            Some(RefreshVerdict::Act),
            "{key}"
        );
        assert_eq!(outcomes(&run, key).fetch, FetchOutcome::Fetched, "{key}");
        assert_eq!(
            ws.refs(dir),
            ws.refs_after_fetch(key, before, &[("refs/heads/main", tip)]),
            "{key}"
        );
    }
    for key in ["oracle", "wpt"] {
        assert_eq!(find_entry(&run.entries, key).refresh, None, "{key}");
        assert_eq!(outcomes(&run, key).fetch, FetchOutcome::NotFetched, "{key}");
    }
    assert_eq!(ws.refs(&oracle), oracle_before);
    assert_eq!(ws.refs(&wpt), wpt_before);
    let mut log = ws.https_log();
    log.sort();
    assert_eq!(log, [https_url("dom"), https_url("lib")]);
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

#[test]
fn a_named_pin_is_refused_and_never_fetched() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("oracle", &[]);
    ws.upstream_commit("oracle", "fork");
    ws.declare_reference(
        "oracle",
        THIRD_PARTY,
        "oracle",
        "branch = \"fork\"\npinned = true",
    );
    let oracle = ws.clone_third_party_over_https("oracle", "oracle", &["--branch", "fork"]);
    ws.upstream_commit("oracle", "fork");
    ws.remote("wpt", &[]);
    ws.declare_reference("wpt", OWNER, "wpt", "pinned = true");
    let wpt = ws.clone_owned("wpt", "wpt", &[]);
    ws.upstream_commit("wpt", "main");
    ws.serve_https();
    ws.write_registry();
    let (oracle_before, wpt_before) = (ws.refs(&oracle), ws.refs(&wpt));

    let run = ws.sync_asked(Refresh::Named, 4);

    for key in ["oracle", "wpt"] {
        let e = find_entry(&run.entries, key);
        assert_eq!(
            e.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::Pinned
            }),
            "{key}"
        );
        assert_eq!(outcomes(&run, key).fetch, FetchOutcome::NotFetched, "{key}");
    }
    assert_eq!(ws.refs(&oracle), oracle_before);
    assert_eq!(ws.refs(&wpt), wpt_before);
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

// --- an origin not over HTTPS: a refresh held, never fetched ---

/// A refresh of `lib` held for its origin not reaching the repo over HTTPS:
/// its own reason, `fix` as said, nothing fetched over any transport, the
/// refs as they were — under sync (with nothing failed, so exit 0) and the
/// preview, `--fetch` or not.
fn assert_held_not_https(
    ws: &FixtureWorkspace,
    lib: &Path,
    fetch_url: &str,
    fix: Option<&OriginFix>,
) {
    let before = ws.refs(lib);
    for refresh in [Refresh::Named, Refresh::References] {
        let run = ws.sync_asked(refresh, 4);
        let e = find_entry(&run.entries, "lib");
        assert_eq!(
            e.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::OriginNotHttps
            }),
            "{refresh:?}"
        );
        assert_eq!(
            e.needs_human,
            [NeedsHuman::OriginNotHttps {
                fetch_url: fetch_url.to_owned(),
                expected: https_url("lib"),
                fix: fix.cloned(),
            }]
        );
        // compared against no remote: nothing to act on
        assert!(e.branches.is_empty(), "{:?}", e.branches);
        assert_eq!(e.fetch_error, None);
        let s = outcomes(&run, "lib");
        assert_eq!(s.fetch, FetchOutcome::NotFetched, "{refresh:?}");
        assert!(s.branches.is_empty(), "{:?}", s.branches);
        // no probe error, fetch, or branch outcome failed: the run exits 0
        assert_eq!(e.probe_error, None);
    }
    for fetch in [false, true] {
        let e = support::take_entry(ws.status_asked(fetch, Refresh::Named), "lib");
        assert_eq!(
            e.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::OriginNotHttps
            })
        );
        assert_eq!(e.fetch_error, None);
    }
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert!(ws.https_refused_log().is_empty());
    assert_eq!(ws.refs(lib), before);
}

/// `origin` the right repo over SSH, as a person's clone might have it:
/// no drift, but a reference is fetched over HTTPS alone.
#[test]
fn a_refresh_whose_origin_is_the_repo_over_ssh_is_held() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    let ssh = format!("git@github.com:{THIRD_PARTY}/lib");
    ws.git(&lib, &["remote", "set-url", "origin", &ssh]);

    assert_held_not_https(&ws, &lib, &ssh, Some(&OriginFix::SetUrl));

    // the control: its origin set as the fix says, it's fetched
    ws.git(&lib, &["remote", "set-url", "origin", &https_url("lib")]);
    let run = ws.sync_asked(Refresh::Named, 4);
    assert_eq!(
        find_entry(&run.entries, "lib").refresh,
        Some(RefreshVerdict::Act)
    );
    assert_eq!(outcomes(&run, "lib").fetch, FetchOutcome::Fetched);
    assert_eq!(ws.https_log(), [https_url("lib")]);
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

/// The user's rewrite of GitHub's HTTPS to SSH, which the fixture's `ssh`
/// would log reaching for: `origin` is the HTTPS URL, but the fetch would
/// go over SSH. Setting the URL can't undo it, so no fix is said.
#[test]
fn a_refresh_rewritten_to_ssh_by_instead_of_is_held() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    ws.git(
        &lib,
        &[
            "config",
            "url.git@github.com:.insteadOf",
            "https://github.com/",
        ],
    );
    let ssh = format!("git@github.com:{THIRD_PARTY}/lib");
    assert_eq!(ws.git(&lib, &["ls-remote", "--get-url", "origin"]), ssh);

    assert_held_not_https(&ws, &lib, &ssh, None);
}

// --- origin drift: a refresh held, never fetched ---

/// A third-party reference whose `origin` is the owner's SSH fork, as a
/// person's checkout of `kit` might be, with no rewrite: a fetch would
/// reach for SSH. Returns the clone.
fn reference_on_a_fork(ws: &mut FixtureWorkspace, name: &str) -> PathBuf {
    let (dir, _) = reference_behind(ws, name, &[]);
    ws.git(
        &dir,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("git@github.com:{OWNER}/{name}"),
        ],
    );
    dir
}

#[test]
fn a_refresh_with_origin_drift_is_held_and_never_fetched() {
    let mut ws = FixtureWorkspace::new();
    let kit = reference_on_a_fork(&mut ws, "kit");
    let before = ws.refs(&kit);

    for refresh in [Refresh::Named, Refresh::References] {
        let run = ws.sync_asked(refresh, 4);

        let e = find_entry(&run.entries, "kit");
        assert_eq!(
            e.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::Entry
            }),
            "{refresh:?}"
        );
        assert!(
            matches!(e.needs_human[..], [NeedsHuman::OriginMismatch { .. }]),
            "{:?}",
            e.needs_human
        );
        // compared against no remote: nothing to act on
        assert!(e.branches.is_empty(), "{:?}", e.branches);
        assert_eq!(e.fetch_error, None);
        let s = outcomes(&run, "kit");
        assert_eq!(s.fetch, FetchOutcome::NotFetched, "{refresh:?}");
        assert!(s.branches.is_empty(), "{:?}", s.branches);
    }
    // nothing reached for, over any transport
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert!(ws.https_refused_log().is_empty());
    assert_eq!(ws.refs(&kit), before);

    // `status kit` previews the same, and `--fetch` fetches nothing
    for fetch in [false, true] {
        let e = support::take_entry(ws.status_asked(fetch, Refresh::Named), "kit");
        assert_eq!(
            e.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::Entry
            })
        );
        assert_eq!(e.fetch_error, None);
    }
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert_eq!(ws.refs(&kit), before);
}

/// An HTTPS fork the fixture's `https` would serve: fetched, it would write
/// the fork's branches as origin's.
#[test]
fn a_refresh_whose_origin_is_an_https_fork_writes_no_fork_refs() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    ws.remote("lib-fork", &[("fork.txt", "fork\n")]);
    ws.upstream_commit("lib-fork", "patched");
    ws.git(
        &lib,
        &["remote", "set-url", "origin", &https_url("lib-fork")],
    );
    let before = ws.refs(&lib);

    let run = ws.sync_asked(Refresh::Named, 4);

    let e = find_entry(&run.entries, "lib");
    assert_eq!(
        e.refresh,
        Some(RefreshVerdict::Held {
            by: RefreshHold::Entry
        })
    );
    assert_eq!(outcomes(&run, "lib").fetch, FetchOutcome::NotFetched);
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
    assert_eq!(ws.refs(&lib), before);
    assert!(!ws.has_ref(&lib, "refs/remotes/origin/patched"));

    // the control: its origin set right, it's fetched
    ws.git(&lib, &["remote", "set-url", "origin", &https_url("lib")]);
    let run = ws.sync_asked(Refresh::Named, 4);
    assert_eq!(outcomes(&run, "lib").fetch, FetchOutcome::Fetched);
    assert_eq!(ws.https_log(), [https_url("lib")]);
}

// --- what a refresh does, and doesn't ---

#[test]
fn a_shallow_reference_moves_to_the_fetched_tip_when_clean() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("spec", &[("a.txt", "a\n")]);
    ws.upstream_commit("spec", "main");
    ws.declare_reference("spec", THIRD_PARTY, "spec", "shallow = true");
    let spec = ws.clone_third_party_over_https("spec", "spec", &["--depth", "1"]);
    ws.assert_shallow(&spec, true);
    let tip = ws.upstream_commit("spec", "main");
    ws.serve_https();
    ws.write_registry();
    ws.assert_clean(&spec);
    let before = ws.refs(&spec);

    let run = ws.sync_asked(Refresh::Named, 4);

    let e = find_entry(&run.entries, "spec");
    assert_eq!(branch(e, "main").relation, Relation::Shallow);
    assert_eq!(
        outcome(&run, "spec", "main"),
        &BranchOutcome::Moved {
            from: before["refs/heads/main"].clone(),
            to: tip.clone(),
        }
    );
    assert_eq!(
        ws.refs(&spec),
        ws.refs_after_fetch("spec", &before, &[("refs/heads/main", &tip)])
    );
    ws.assert_shallow(&spec, true);
    ws.assert_head(&spec, Some("main"));
    ws.assert_clean(&spec);
    assert!(spec.join("upstream-main.txt").is_file());
    ws.assert_upstream(&spec, "main", "refs/remotes/origin/main");
}

#[test]
fn a_dirty_reference_is_fetched_and_held() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    write(&lib, "scratch.txt", "mine\n");
    ws.assert_porcelain(&lib, &["?? scratch.txt"]);
    let before = ws.refs(&lib);

    let run = ws.sync_asked(Refresh::Named, 4);

    assert_eq!(outcomes(&run, "lib").fetch, FetchOutcome::Fetched);
    assert_eq!(
        outcome(&run, "lib", "main"),
        &BranchOutcome::Held {
            action: ff(1),
            by: BranchSyncHold::DirtyCheckout
        }
    );
    // the fetch alone: the branch where it was
    assert_eq!(ws.refs(&lib), ws.refs_after_fetch("lib", &before, &[]));
    ws.assert_porcelain(&lib, &["?? scratch.txt"]);
}

#[test]
fn local_commits_on_a_reference_are_never_moved_or_pushed() {
    let mut ws = FixtureWorkspace::new();
    // diverged: a local commit, and upstream moved
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    ws.commit(&lib, "local");
    // ahead only: local commits, upstream still
    ws.remote("dom", &[]);
    ws.declare_reference("dom", THIRD_PARTY, "dom", "");
    let dom = ws.clone_third_party_over_https("dom", "dom", &[]);
    ws.commit(&dom, "local");
    // shallow, a local commit off the old tip, upstream moved
    ws.remote("spec", &[]);
    ws.upstream_commit("spec", "main");
    ws.declare_reference("spec", THIRD_PARTY, "spec", "shallow = true");
    let spec = ws.clone_third_party_over_https("spec", "spec", &["--depth", "1"]);
    ws.commit(&spec, "local");
    ws.upstream_commit("spec", "main");
    ws.write_registry();
    let remote_heads = |name: &str| ws.git(&ws.bare(name), &["rev-parse", "main"]);
    let dom_remote = remote_heads("dom");
    let befores: BTreeMap<&str, _> = [
        ("lib", ws.refs(&lib)),
        ("dom", ws.refs(&dom)),
        ("spec", ws.refs(&spec)),
    ]
    .into_iter()
    .collect();

    let run = ws.sync_asked(Refresh::Named, 4);

    let e = |key| find_entry(&run.entries, key);
    assert_eq!(
        branch(e("lib"), "main").verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(branch(e("dom"), "main").verdict, Verdict::LocalOnly);
    assert_eq!(
        branch(e("spec"), "main").verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::ShallowLocalWork
        }
    );
    assert_eq!(
        outcome(&run, "lib", "main"),
        &BranchOutcome::NeedsHuman {
            reason: BranchNeedsHuman::Diverged
        }
    );
    assert_eq!(outcome(&run, "dom", "main"), &BranchOutcome::Untouched);
    for (key, dir) in [("lib", &lib), ("dom", &dom), ("spec", &spec)] {
        assert_eq!(outcomes(&run, key).fetch, FetchOutcome::Fetched, "{key}");
        // its push URL never read: no push to hold
        assert!(
            e(key).needs_human.is_empty(),
            "{key}: {:?}",
            e(key).needs_human
        );
        // fetched, and nothing else moved
        assert_eq!(
            ws.refs(dir),
            ws.refs_after_fetch(key, &befores[key], &[]),
            "{key}"
        );
    }
    // never pushed
    assert_eq!(remote_heads("dom"), dom_remote);
    assert!(ws.ssh_log().is_empty(), "{:?}", ws.ssh_log());
}

#[test]
fn an_owned_reference_syncs_as_before_named_or_not() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("html", &[]);
    ws.declare_reference("html", OWNER, "html", "");
    let html = ws.clone_owned("html", "html", &[]);
    ws.upstream_commit("html", "main");
    ws.write_registry();

    for refresh in [Refresh::Unasked, Refresh::Named, Refresh::References] {
        let tip = ws.upstream_commit("html", "main");
        let before = ws.refs(&html);
        let run = ws.sync_asked(refresh, 4);
        let e = find_entry(&run.entries, "html");
        assert_eq!(e.refresh, None, "{refresh:?}");
        assert_eq!(outcomes(&run, "html").fetch, FetchOutcome::Fetched);
        assert!(
            matches!(outcome(&run, "html", "main"), BranchOutcome::FastForwarded { to, .. } if *to == tip),
            "{refresh:?}: {:?}",
            run.outcomes
        );
        assert_eq!(
            ws.refs(&html),
            ws.refs_after_fetch("html", &before, &[("refs/heads/main", &tip)])
        );
    }
    assert!(ws.https_log().is_empty());
}

// --- partial clones: the checkout fetches the blobs it needs ---

#[test]
fn a_sparse_reference_fast_forwards_fetching_its_cones_blobs() {
    let mut ws = FixtureWorkspace::new();
    ws.remote(
        "lib",
        &[
            ("css/a.css", "a {}\n"),
            ("html/c.html", "<p>\n"),
            ("top.txt", "top\n"),
        ],
    );
    ws.declare_reference("lib", THIRD_PARTY, "lib", "sparse = \"css\"");
    ws.serve_https();
    ws.write_registry();
    let run = ws.sync();
    assert!(matches!(
        outcomes(&run, "lib").clone,
        Some(CloneOutcome::Cloned { .. })
    ));
    let lib = ws.dir("lib");
    assert_eq!(files(&lib), ["README", "css/a.css", "top.txt"]);
    assert_eq!(missing_objects(&ws, &lib), 1);
    // upstream changes a file in the cone and one outside it
    let up = ws.upstream("lib");
    write(&up, "css/a.css", "a { color: red }\n");
    write(&up, "html/c.html", "<p>new\n");
    ws.git(&up, &["commit", "-q", "-am", "both"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    let tip = ws.git(&up, &["rev-parse", "HEAD"]);
    let before = ws.refs(&lib);
    let clone_log = ws.https_log().len();

    let run = ws.sync_asked(Refresh::Named, 4);

    assert_eq!(
        outcome(&run, "lib", "main"),
        &BranchOutcome::FastForwarded {
            from: before["refs/heads/main"].clone(),
            to: tip.clone(),
        }
    );
    assert_eq!(
        ws.refs(&lib),
        ws.refs_after_fetch("lib", &before, &[("refs/heads/main", &tip)])
    );
    ws.assert_clean(&lib);
    assert_eq!(
        std::fs::read_to_string(lib.join("css/a.css")).unwrap(),
        "a { color: red }\n"
    );
    // the cone alone checked out; the blobs outside it never fetched, the
    // old one and the new
    assert_eq!(files(&lib), ["README", "css/a.css", "top.txt"]);
    assert_eq!(missing_objects(&ws, &lib), 2);
    // the refresh's fetch, then the checkout's fetch of the cone's blob
    assert_eq!(
        ws.https_log()[clone_log..],
        [https_url("lib"), https_url("lib")]
    );
}

/// wpt's recipe without the pin: an owned fork, shallow, sparse, on its
/// `fork` branch — its move's checkout fetches the cone's blobs over SSH.
#[test]
fn an_unpinned_sparse_fork_moves_fetching_its_cones_blobs() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("wpt", &[("css/a.css", "a {}\n"), ("html/c.html", "<p>\n")]);
    ws.upstream_commit("wpt", "fork");
    ws.declare_reference(
        "wpt",
        OWNER,
        "wpt",
        "branch = \"fork\"\nshallow = true\nsparse = \"css\"",
    );
    ws.write_registry();
    let run = ws.sync();
    assert!(matches!(
        outcomes(&run, "wpt").clone,
        Some(CloneOutcome::Cloned { .. })
    ));
    let wpt = ws.dir("wpt");
    let up = ws.upstream("wpt");
    ws.git(&up, &["checkout", "-q", "fork"]);
    write(&up, "css/a.css", "a { color: red }\n");
    ws.git(&up, &["commit", "-q", "-am", "css"]);
    ws.git(&up, &["push", "-q", "origin", "fork"]);
    let tip = ws.git(&up, &["rev-parse", "HEAD"]);
    let before = ws.refs(&wpt);
    let calls = ws.ssh_log().len();

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "wpt", "fork"),
        &BranchOutcome::Moved {
            from: before["refs/heads/fork"].clone(),
            to: tip.clone(),
        }
    );
    ws.assert_head(&wpt, Some("fork"));
    ws.assert_clean(&wpt);
    ws.assert_shallow(&wpt, true);
    assert_eq!(ws.git(&wpt, &["rev-parse", "fork"]), tip);
    assert_eq!(
        std::fs::read_to_string(wpt.join("css/a.css")).unwrap(),
        "a { color: red }\n"
    );
    assert_eq!(files(&wpt), ["README", "css/a.css", "upstream-fork.txt"]);
    // the fetch, then the checkout's fetch of the cone's blob
    let log = ws.ssh_log();
    assert_eq!(log.len() - calls, 2, "{log:?}");
    assert!(
        log[calls..].iter().all(|l| l.contains("git-upload-pack")),
        "{log:?}"
    );
}

/// Origin rewritten between classifying and the checkout — an `insteadOf`
/// sending it to another repo — holds the move before its lazy fetch
/// reaches anything: origin is read again, as git resolves it, right
/// before.
#[test]
fn origin_rewritten_before_a_partial_checkout_holds_it() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("wpt", &[("css/a.css", "a {}\n"), ("html/c.html", "<p>\n")]);
    ws.upstream_commit("wpt", "fork");
    ws.declare_reference(
        "wpt",
        OWNER,
        "wpt",
        "branch = \"fork\"\nshallow = true\nsparse = \"css\"",
    );
    ws.write_registry();
    ws.sync();
    let wpt = ws.dir("wpt");
    let up = ws.upstream("wpt");
    ws.git(&up, &["checkout", "-q", "fork"]);
    write(&up, "css/a.css", "a { color: red }\n");
    ws.git(&up, &["commit", "-q", "-am", "css"]);
    ws.git(&up, &["push", "-q", "origin", "fork"]);
    let tip = ws.git(&up, &["rev-parse", "HEAD"]);
    let was = ws.git(&wpt, &["rev-parse", "fork"]);
    let calls = ws.ssh_log().len();
    let env = ws.env();
    let rewrite = "url.git@github.com:me/other.insteadOf";
    // after the fetch and classifying, right before the move
    let reads = AtomicUsize::new(0);
    let read = || {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            let out = Command::new("git")
                .env_clear()
                .envs(env.iter().map(|(k, v)| (k, v)))
                .current_dir(&wpt)
                .args(["config", rewrite, "git@github.com:me/wpt"])
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        LiveSessions::Known(vec![])
    };

    let run = ws.sync_with(4, &read);

    let e = find_entry(&run.entries, "wpt");
    assert_eq!(
        branch(e, "fork").verdict,
        Verdict::Act {
            action: SyncAction::Move
        }
    );
    assert_eq!(
        outcome(&run, "wpt", "fork"),
        &BranchOutcome::Held {
            action: SyncAction::Move,
            by: BranchSyncHold::Changed,
        }
    );
    assert_eq!(ws.git(&wpt, &["rev-parse", "fork"]), was);
    ws.assert_clean(&wpt);
    assert_eq!(
        std::fs::read_to_string(wpt.join("css/a.css")).unwrap(),
        "a {}\n"
    );
    // the fetch alone reached a remote
    let log = ws.ssh_log();
    assert_eq!(log.len() - calls, 1, "{log:?}");
    assert!(log.iter().all(|l| !l.contains("me/other")), "{log:?}");

    // the rewrite gone, the move goes on
    ws.git(&wpt, &["config", "--unset", rewrite]);
    let run = ws.sync();
    assert_eq!(
        outcome(&run, "wpt", "fork"),
        &BranchOutcome::Moved { from: was, to: tip }
    );
}

/// wpt as an owned fork, shallow and sparse on `fork`, synced once, and a
/// commit to its cone upstream: the fork's clone, the new tip, and the
/// branch's commit before it.
fn sparse_fork_behind(ws: &mut FixtureWorkspace) -> (PathBuf, String, String) {
    ws.remote("wpt", &[("css/a.css", "a {}\n"), ("html/c.html", "<p>\n")]);
    ws.upstream_commit("wpt", "fork");
    ws.declare_reference(
        "wpt",
        OWNER,
        "wpt",
        "branch = \"fork\"\nshallow = true\nsparse = \"css\"",
    );
    ws.write_registry();
    ws.sync();
    let wpt = ws.dir("wpt");
    let up = ws.upstream("wpt");
    ws.git(&up, &["checkout", "-q", "fork"]);
    write(&up, "css/a.css", "a { color: red }\n");
    ws.git(&up, &["commit", "-q", "-am", "css"]);
    ws.git(&up, &["push", "-q", "origin", "fork"]);
    let tip = ws.git(&up, &["rev-parse", "HEAD"]);
    let was = ws.git(&wpt, &["rev-parse", "fork"]);
    (wpt, tip, was)
}

/// A rewrite that stays: origin configured as the repo over HTTPS, and a
/// permanent `insteadOf` sending it over SSH. The checkout's lazy fetch
/// takes the transport git connects over, as the fetch does, so the move
/// goes on, never held as changed.
#[test]
fn a_permanent_rewrite_of_a_partial_clones_origin_is_its_transport() {
    let mut ws = FixtureWorkspace::new();
    let (wpt, tip, was) = sparse_fork_behind(&mut ws);
    let https = format!("https://github.com/{OWNER}/wpt");
    ws.git(&wpt, &["remote", "set-url", "origin", &https]);
    let rewrite = format!("url.git@github.com:{OWNER}/wpt.insteadOf");
    ws.git(&wpt, &["config", &rewrite, &https]);
    assert_eq!(
        ws.git(&wpt, &["ls-remote", "--get-url", "origin"]),
        support::owned_origin("wpt")
    );
    let calls = ws.ssh_log().len();

    let run = ws.sync();

    assert_eq!(
        outcome(&run, "wpt", "fork"),
        &BranchOutcome::Moved { from: was, to: tip }
    );
    assert!(find_entry(&run.entries, "wpt").needs_human.is_empty());
    assert_eq!(
        std::fs::read_to_string(wpt.join("css/a.css")).unwrap(),
        "a { color: red }\n"
    );
    // the fetch, then the checkout's fetch of the cone's blob, both over SSH
    let log = ws.ssh_log();
    assert_eq!(log.len() - calls, 2, "{log:?}");
    assert!(ws.https_refused_log().is_empty());
}

/// A partial clone whose fetch resolves to a transport its lazy fetch may
/// not take — the repo over `git://`, a rewrite's doing — is a person's,
/// named, never fetched or moved.
#[test]
fn a_partial_clone_fetching_over_another_transport_needs_a_person() {
    let mut ws = FixtureWorkspace::new();
    let (wpt, _, was) = sparse_fork_behind(&mut ws);
    let git_url = format!("git://github.com/{OWNER}/wpt");
    let rewrite = format!("url.{git_url}.insteadOf");
    ws.git(&wpt, &["config", &rewrite, &support::owned_origin("wpt")]);
    let before = ws.refs(&wpt);
    let calls = ws.ssh_log().len();

    let run = ws.sync();

    let e = find_entry(&run.entries, "wpt");
    assert_eq!(
        e.needs_human,
        [NeedsHuman::FetchUrlMismatch {
            fetch_url: git_url,
            expected: support::owned_origin("wpt"),
            fix: None,
        }]
    );
    // unfetched, the branch reads as the last fetch left it: nothing to do
    assert_eq!(outcomes(&run, "wpt").fetch, FetchOutcome::NotFetched);
    assert_eq!(outcome(&run, "wpt", "fork"), &BranchOutcome::Untouched);
    assert_eq!(ws.refs(&wpt), before);
    assert_eq!(ws.git(&wpt, &["rev-parse", "fork"]), was);
    assert_eq!(ws.ssh_log().len(), calls);
}

/// A promisor remote added between classifying and the checkout holds the
/// move before its lazy fetch: git would ask that remote too, so whether
/// one is configured is read again right before, with origin.
#[test]
fn a_promisor_remote_added_before_a_partial_checkout_holds_it() {
    let mut ws = FixtureWorkspace::new();
    let (wpt, _, was) = sparse_fork_behind(&mut ws);
    let calls = ws.ssh_log().len();
    let env = ws.env();
    // after the fetch and classifying, right before the move
    let reads = AtomicUsize::new(0);
    let read = || {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            for args in [
                &["remote", "add", "mirror", "git@github.com:me/other"][..],
                &["config", "remote.mirror.promisor", "true"],
            ] {
                let out = Command::new("git")
                    .env_clear()
                    .envs(env.iter().map(|(k, v)| (k, v)))
                    .current_dir(&wpt)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(out.status.success());
            }
        }
        LiveSessions::Known(vec![])
    };

    let run = ws.sync_with(4, &read);

    assert_eq!(
        outcome(&run, "wpt", "fork"),
        &BranchOutcome::Held {
            action: SyncAction::Move,
            by: BranchSyncHold::Changed,
        }
    );
    assert_eq!(ws.git(&wpt, &["rev-parse", "fork"]), was);
    ws.assert_clean(&wpt);
    assert_eq!(
        std::fs::read_to_string(wpt.join("css/a.css")).unwrap(),
        "a {}\n"
    );
    // the fetch alone reached a remote
    let log = ws.ssh_log();
    assert_eq!(log.len() - calls, 1, "{log:?}");
}

/// A promisor remote besides origin keeps the lazy fetch off: git would
/// ask it too, and the tool reaches origin alone.
#[test]
fn a_second_promisor_remote_keeps_the_checkouts_fetch_off() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("lib", &[("css/a.css", "a {}\n"), ("html/c.html", "<p>\n")]);
    ws.declare_reference("lib", THIRD_PARTY, "lib", "sparse = \"css\"");
    ws.serve_https();
    ws.write_registry();
    ws.sync();
    let lib = ws.dir("lib");
    ws.git(&lib, &["remote", "add", "mirror", &https_url("lib-mirror")]);
    ws.git(&lib, &["config", "remote.mirror.promisor", "true"]);
    let up = ws.upstream("lib");
    write(&up, "css/a.css", "a { color: red }\n");
    ws.git(&up, &["commit", "-q", "-am", "css"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    let before = ws.refs(&lib);
    let log = ws.https_log().len();

    let run = ws.sync_asked(Refresh::Named, 4);

    assert!(
        matches!(
            outcome(&run, "lib", "main"),
            BranchOutcome::Failed { message, .. } if message.contains("promisor remote")
        ),
        "{:?}",
        run.outcomes
    );
    // the fetch alone reached a remote; the branch and files as they were
    assert_eq!(ws.https_log()[log..], [https_url("lib")]);
    assert_eq!(ws.refs(&lib), ws.refs_after_fetch("lib", &before, &[]));
    ws.assert_clean(&lib);
    assert_eq!(
        std::fs::read_to_string(lib.join("css/a.css")).unwrap(),
        "a {}\n"
    );
}

// --- a missing entry already cloned under another name ---

#[test]
fn a_missing_entry_cloned_under_another_name_is_held() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    // the checkout, renamed by hand
    ws.clone_owned("app-old", "app", &[]);
    // the tool's own leftover beside it holds nothing
    std::fs::create_dir(ws.root().join(".blake3.repos-clone-1-0123456789abcdef")).unwrap();
    ws.remote("blake3", &[]);
    ws.declare_repo("blake3", "blake3", "");
    ws.write_registry();
    let listing = root_listing(&ws);

    // status previews it, once the scan has run
    let entries = ws.status_scanned();
    let e = find_entry(&entries, "app");
    assert_eq!(e.presence, Presence::Missing);
    assert_eq!(
        e.needs_human,
        [NeedsHuman::ClonedUnregistered {
            dir: "app-old".into()
        }]
    );
    assert!(matches!(
        e.clone,
        Some(CloneVerdict::Held {
            by: CloneHold::Entry,
            ..
        })
    ));
    assert!(matches!(
        find_entry(&entries, "blake3").clone,
        Some(CloneVerdict::Act { .. })
    ));

    let run = ws.sync_scanned();

    assert_eq!(
        outcomes(&run, "app").clone,
        Some(CloneOutcome::Held {
            by: CloneSyncHold::Entry
        })
    );
    assert!(matches!(
        outcomes(&run, "blake3").clone,
        Some(CloneOutcome::Cloned { .. })
    ));
    // nothing made for app, and no remote asked for it
    let mut want = listing;
    want.push("blake3".into());
    want.sort();
    assert_eq!(root_listing(&ws), want);
    assert!(
        ws.ssh_log().iter().all(|l| !l.contains("/app")),
        "{:?}",
        ws.ssh_log()
    );

    // without the scan's dirs it isn't seen: cloned
    let run = ws.sync();
    assert!(matches!(
        outcomes(&run, "app").clone,
        Some(CloneOutcome::Cloned { .. })
    ));
}

/// A repo renamed from `_` to `-`: the registry names the new name, the
/// checkout at the root still the old one, its origin the old URL.
#[test]
fn a_missing_entry_cloned_under_its_old_name_is_held() {
    let mut ws = FixtureWorkspace::new();
    let name = "vscode-extension-tsv-format";
    ws.remote(name, &[]);
    ws.declare_repo(name, name, "");
    let old = ws.clone_as(
        "vscode_extension_tsv_format",
        name,
        &format!("git@github.com:{OWNER}/vscode_extension_tsv_format"),
        &[],
    );
    // another name, not a rename of it: a person's to resolve, never held
    ws.remote("podcast", &[]);
    ws.declare_repo("podcast", "podcast", "");
    ws.clone_as(
        "old_podcast",
        "podcast",
        &format!("git@github.com:{OWNER}/old_podcast_notes"),
        &[],
    );
    ws.write_registry();
    let before = ws.refs(&old);

    let entries = ws.status_scanned();
    let e = find_entry(&entries, name);
    assert_eq!(
        e.needs_human,
        [NeedsHuman::ClonedUnregistered {
            dir: "vscode_extension_tsv_format".into()
        }]
    );
    assert!(matches!(
        e.clone,
        Some(CloneVerdict::Held {
            by: CloneHold::Entry,
            ..
        })
    ));
    assert!(matches!(
        find_entry(&entries, "podcast").clone,
        Some(CloneVerdict::Act { .. })
    ));

    let run = ws.sync_scanned();
    assert_eq!(
        outcomes(&run, name).clone,
        Some(CloneOutcome::Held {
            by: CloneSyncHold::Entry
        })
    );
    assert!(!ws.dir(name).exists());
    assert_eq!(ws.refs(&old), before);
}

// --- the preview ---

#[test]
fn status_previews_a_refresh_and_fetches_only_under_fetch() {
    let mut ws = FixtureWorkspace::new();
    let (lib, tip) = reference_behind(&mut ws, "lib", &[]);
    let before = ws.refs(&lib);

    // local refs: the relation as they say, nothing reached for
    for refresh in [Refresh::Named, Refresh::References] {
        let e = support::take_entry(ws.status_asked(false, refresh), "lib");
        assert_eq!(e.refresh, Some(RefreshVerdict::Act), "{refresh:?}");
        assert_eq!(branch(&e, "main").relation, Relation::InSync);
        assert_eq!(branch(&e, "main").verdict, Verdict::Quiet);
    }
    assert!(ws.https_log().is_empty());
    // unasked, it's quiet
    let e = support::take_entry(ws.status_asked(false, Refresh::Unasked), "lib");
    assert_eq!(e.refresh, None);
    assert!(e.branches.is_empty(), "{:?}", e.branches);

    // `--fetch`: fetched, and what sync would do, nothing else
    let e = support::take_entry(ws.status_asked(true, Refresh::Named), "lib");
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").verdict, Verdict::Act { action: ff(1) });
    assert_eq!(ws.https_log(), [https_url("lib")]);
    assert_eq!(ws.refs(&lib), ws.refs_after_fetch("lib", &before, &[]));
    assert_ne!(ws.git(&lib, &["rev-parse", "main"]), tip);
}

/// A probe that fails before the config is read (a non-UTF-8 value in
/// it) never fetches: a named reference's refresh isn't previewed as one,
/// while a named pin is still refused.
#[test]
fn a_refresh_whose_config_is_unread_is_not_previewed() {
    let mut ws = FixtureWorkspace::new();
    let (lib, _) = reference_behind(&mut ws, "lib", &[]);
    ws.remote("pin", &[]);
    ws.declare_reference("pin", THIRD_PARTY, "pin", "pinned = true");
    let pin = ws.clone_third_party_over_https("pin", "pin", &[]);
    ws.write_registry();
    for dir in [&lib, &pin] {
        let config = dir.join(".git/config");
        let mut bytes = std::fs::read(&config).unwrap();
        bytes.extend_from_slice(b"[branch \"main\"]\n\tdescription = \xff\n");
        std::fs::write(&config, bytes).unwrap();
    }
    for fetch in [false, true] {
        let entries = ws.status_asked(fetch, Refresh::Named);
        let e = find_entry(&entries, "lib");
        let kind = e.probe_error.as_ref().map(|p| p.kind);
        assert_eq!(kind, Some(ProbeErrorKind::ConfigUnreadable), "{e:?}");
        assert_eq!(e.refresh, None, "{fetch}");
        let p = find_entry(&entries, "pin");
        let kind = p.probe_error.as_ref().map(|p| p.kind);
        assert_eq!(kind, Some(ProbeErrorKind::ConfigUnreadable), "{p:?}");
        assert_eq!(
            p.refresh,
            Some(RefreshVerdict::Held {
                by: RefreshHold::Pinned
            })
        );
    }
    assert!(ws.https_log().is_empty(), "{:?}", ws.https_log());
}

#[test]
fn refresh_outcomes_are_the_same_whatever_the_jobs() {
    let build = || {
        let mut ws = FixtureWorkspace::new();
        reference_behind(&mut ws, "lib", &[]);
        let (dom, _) = reference_behind(&mut ws, "dom", &[]);
        write(&dom, "scratch.txt", "x\n");
        let (spec, _) = reference_behind(&mut ws, "spec", &[]);
        ws.commit(&spec, "local");
        ws.remote("pin", &[]);
        ws.declare_reference("pin", THIRD_PARTY, "pin", "pinned = true");
        ws.clone_third_party_over_https("pin", "pin", &[]);
        ws.write_registry();
        ws
    };
    let (one, many) = (build(), build());
    let serial = one.sync_asked(Refresh::Named, 1);
    let parallel = many.sync_asked(Refresh::Named, 16);
    // the ids differ by workspace: compare the kinds
    let kinds = |run: &SyncRun| -> Vec<(String, String, Vec<String>)> {
        run.outcomes
            .iter()
            .map(|e| {
                (
                    e.key.clone(),
                    format!("{:?}", e.fetch),
                    e.branches
                        .iter()
                        .map(|b| match &b.outcome {
                            BranchOutcome::FastForwarded { .. } => "ff".to_owned(),
                            o => format!("{o:?}"),
                        })
                        .collect(),
                )
            })
            .collect()
    };
    assert_eq!(kinds(&serial), kinds(&parallel));
    let refreshes = |run: &SyncRun| -> Vec<Option<RefreshVerdict>> {
        run.entries.iter().map(|e| e.refresh).collect()
    };
    assert_eq!(refreshes(&serial), refreshes(&parallel));
    assert_eq!(
        kinds(&serial),
        [
            (
                "dom".into(),
                "Fetched".into(),
                vec![format!(
                    "{:?}",
                    BranchOutcome::Held {
                        action: ff(1),
                        by: BranchSyncHold::DirtyCheckout
                    }
                )]
            ),
            ("lib".into(), "Fetched".into(), vec!["ff".into()]),
            ("pin".into(), "NotFetched".into(), vec![]),
            (
                "spec".into(),
                "Fetched".into(),
                vec![format!(
                    "{:?}",
                    BranchOutcome::NeedsHuman {
                        reason: BranchNeedsHuman::Diverged
                    }
                )]
            ),
        ]
    );
}
