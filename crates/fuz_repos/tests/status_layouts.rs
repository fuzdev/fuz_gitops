//! Clone layouts from real repos: shallow clones (shallow-root subtraction),
//! cone-sparse, partial clones probed with no lazy fetch, and an upstream
//! outside a narrowed refspec.

mod support;

use fuz_repos::state::{
    BranchNeedsHuman, Head, Presence, ProbeErrorKind, Relation, SyncAction, Verdict,
};
use support::{FixtureWorkspace, branch, find_entry};

/// Every object reachable from a ref, missing ones marked `?`.
fn objects(ws: &FixtureWorkspace, repo: &std::path::Path) -> String {
    ws.git(repo, &["rev-list", "--objects", "--all", "--missing=print"])
}

/// Objects the partial clone lacks.
fn missing_objects(ws: &FixtureWorkspace, repo: &std::path::Path) -> usize {
    objects(ws, repo)
        .lines()
        .filter(|l| l.starts_with('?'))
        .count()
}

#[test]
fn shallow_clones_subtract_their_roots() {
    let mut ws = FixtureWorkspace::new();
    for name in ["at_tip", "moved", "local", "deeper"] {
        ws.remote(name, &[]);
        ws.declare_repo(name, name, "");
    }
    // at the fetched tip
    let at_tip = ws.clone_owned("at_tip", "at_tip", &["--depth", "1"]);
    ws.assert_track(&at_tip, "main", "");
    // the tip moved and a second depth-1 fetch landed it: unconnected to
    // local history, so plain git says ahead 1, behind 1
    let moved = ws.clone_owned("moved", "moved", &["--depth", "1"]);
    ws.upstream_commit("moved", "main");
    ws.git(&moved, &["fetch", "-q", "--depth", "1", "origin"]);
    ws.assert_track(&moved, "main", "[ahead 1, behind 1]");
    assert_eq!(
        std::fs::read_to_string(moved.join(".git/shallow"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    // a commit made on the fetched tip
    let local = ws.clone_owned("local", "local", &["--depth", "1"]);
    ws.commit(&local, "local");
    ws.assert_track(&local, "main", "[ahead 1]");
    // a hand-made deeper clone: its non-root commit reads as local work, so
    // a miscount can only push the branch toward a human, never a move
    ws.upstream_commit("deeper", "main");
    let deeper = ws.clone_owned("deeper", "deeper", &["--depth", "2"]);
    ws.upstream_commit("deeper", "main");
    ws.git(&deeper, &["fetch", "-q", "--depth", "1", "origin"]);
    ws.assert_track(&deeper, "main", "[ahead 2, behind 1]");
    for name in ["at_tip", "moved", "local", "deeper"] {
        ws.assert_shallow(&ws.dir(name), true);
        ws.assert_clean(&ws.dir(name));
    }

    let entries = ws.status();
    for e in &entries {
        assert!(e.layout.as_ref().unwrap().shallow, "{}", e.key);
        assert!(e.needs_human.is_empty(), "{}: {:?}", e.key, e.needs_human);
    }

    let at_tip = branch(find_entry(&entries, "at_tip"), "main");
    assert_eq!(at_tip.relation, Relation::InSync);
    assert_eq!(at_tip.verdict, Verdict::Quiet);

    let moved = branch(find_entry(&entries, "moved"), "main");
    assert_eq!(moved.relation, Relation::Shallow);
    assert_eq!(moved.unique_commits, 0);
    // checked out in a clean checkout: nothing holds the move
    assert_eq!(
        moved.verdict,
        Verdict::Act {
            action: SyncAction::Move
        }
    );

    let local = branch(find_entry(&entries, "local"), "main");
    assert_eq!(local.relation, Relation::Ahead { commits: 1 });
    assert_eq!(local.unique_commits, 1);
    assert_eq!(
        local.verdict,
        Verdict::Act {
            action: SyncAction::Push { commits: 1 }
        }
    );

    let deeper = branch(find_entry(&entries, "deeper"), "main");
    assert_eq!(deeper.relation, Relation::Shallow);
    assert_eq!(deeper.unique_commits, 1);
    assert_eq!(
        deeper.verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::ShallowLocalWork
        }
    );
}

#[test]
fn a_shallow_fetch_keeps_depth_one() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[]);
    ws.declare_repo("app", "app", "");
    ws.upstream_commit("app", "main");
    let app = ws.clone_owned("app", "app", &["--depth", "1"]);
    ws.upstream_commit("app", "main");
    ws.assert_shallow(&app, true);
    ws.assert_count(&app, &["--all"], 1);

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    // the fetch deepened nothing: the new tip is another root
    ws.assert_count(&app, &["refs/remotes/origin/main"], 1);
    ws.assert_track(&app, "main", "[ahead 1, behind 1]");
    let main = branch(&e, "main");
    assert_eq!(main.relation, Relation::Shallow);
    assert_eq!(
        main.verdict,
        Verdict::Act {
            action: SyncAction::Move
        }
    );
}

#[test]
fn cone_sparse_partial_clone() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("wide", &[("keep/a.txt", "a\n"), ("skip/b.txt", "b\n")]);
    ws.declare_reference("wide", support::THIRD_PARTY, "wide", "sparse = \"keep\"");
    let wide = ws.clone_third_party("wide", "wide", &["--filter=blob:none", "--sparse"]);
    ws.git(&wide, &["sparse-checkout", "set", "--cone", "keep"]);
    assert!(wide.join("keep/a.txt").is_file());
    assert!(!wide.join("skip").exists());
    assert_eq!(ws.git(&wide, &["config", "core.sparseCheckout"]), "true");
    ws.assert_clean(&wide);

    let e = ws.entry("wide");
    assert_eq!(e.presence, Presence::Present);
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let layout = e.layout.as_ref().unwrap();
    assert!(layout.sparse);
    assert!(!layout.shallow);
    assert_eq!(layout.partial_filter.as_deref(), Some("blob:none"));
    assert!(e.checkouts[0].uncommitted.is_clean());
}

/// The everyday partial layout: the probe needs none of the blobs the clone
/// lacks, so it succeeds with the remote gone. The lazy-fetch guard itself is
/// pinned by the tree:0 test, where the probe does need a missing object.
#[test]
fn a_sparse_partial_clone_probes_without_its_missing_blobs() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("keep/a.txt", "a\n"), ("skip/b.txt", "b\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--filter=blob:none", "--sparse"]);
    ws.git(&app, &["sparse-checkout", "set", "--cone", "keep"]);
    // blobs outside the cone were never fetched
    let missing = missing_objects(&ws, &app);
    assert!(missing > 0, "the partial clone should lack blobs");
    // a local commit and a local branch, so the probe walks history
    ws.commit(&app, "keep/local");
    ws.git(&app, &["branch", "wip"]);
    ws.assert_track(&app, "main", "[ahead 1]");
    // the remote becomes unreachable: any fetch would now fail
    std::fs::rename(ws.bare("app"), ws.outside("unreachable.git")).unwrap();

    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    assert_eq!(e.presence, Presence::Present);
    assert_eq!(
        e.layout.as_ref().unwrap().partial_filter.as_deref(),
        Some("blob:none")
    );
    assert_eq!(branch(&e, "main").relation, Relation::Ahead { commits: 1 });
    assert_eq!(branch(&e, "wip").unique_commits, 1);
    assert!(e.checkouts[0].uncommitted.is_clean());
    // nothing was fetched
    assert_eq!(missing_objects(&ws, &app), missing);
}

#[test]
fn a_probe_that_needs_a_missing_object_fails_rather_than_fetching() {
    // a tree:0 clone with no checkout lacks HEAD's tree, which `status`
    // needs: git would fetch it on demand, and the remote is reachable
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("dir/a.txt", "a\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--filter=tree:0", "--no-checkout"]);
    let before = objects(&ws, &app);
    assert_eq!(missing_objects(&ws, &app), 1, "{before}");
    assert!(ws.bare("app").is_dir());

    let e = ws.entry("app");
    // nothing fetched: the probe refused instead of guessing
    assert_eq!(objects(&ws, &app), before);
    assert_eq!(e.presence, Presence::Present);
    let error = e.probe_error.as_ref().unwrap();
    assert_eq!(error.kind, ProbeErrorKind::GitFailed);
    assert!(error.message.contains("bad tree object HEAD"), "{e:?}");
    // the layout survives the failure, so the hint keys on the filter
    assert_eq!(
        e.layout.as_ref().and_then(|l| l.partial_filter.as_deref()),
        Some("tree:0")
    );
    assert!(e.probe_failed_partial());
}

/// The partial-clone hint's advice works: `checkout`, run by a person (lazy
/// fetching allowed), fetches what the probe lacked and fills the checkout —
/// clean, not an empty index reading every file as a staged deletion. Run
/// again on the filled checkout, it changes nothing.
#[test]
fn a_partial_clone_probes_clean_once_checked_out() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("app", &[("dir/a.txt", "a\n")]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &["--filter=tree:0", "--no-checkout"]);
    assert_eq!(missing_objects(&ws, &app), 1);
    assert!(ws.entry("app").probe_failed_partial());

    ws.git(&app, &["checkout"]);
    ws.assert_clean(&app);
    let e = ws.entry("app");
    assert_eq!(e.probe_error, None);
    assert!(!e.probe_failed_partial());
    assert!(e.checkouts[0].uncommitted.is_clean(), "{:?}", e.checkouts);
    assert_eq!(
        e.checkouts[0].head,
        Head::Branch {
            name: "main".into()
        }
    );

    // harmless on a checkout that's already there, dirt and all
    let head = ws.git(&app, &["rev-parse", "HEAD"]);
    support::write(&app, "dir/a.txt", "edited\n");
    ws.git(&app, &["checkout"]);
    ws.assert_porcelain(&app, &[" M dir/a.txt"]);
    assert_eq!(ws.git(&app, &["rev-parse", "HEAD"]), head);
    ws.assert_head(&app, Some("main"));
}

/// A failure outside a partial clone gets no partial-clone hint.
#[test]
fn a_full_clone_probe_failure_is_not_a_partial_one() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    // a local commit's tree, loose, deleted: `status` fails on HEAD's tree
    // as the tree:0 clone's does, and there's no promisor to fetch it from
    ws.commit(&app, "local");
    // `HEAD:` names HEAD's root tree
    let tree = ws.git(&app, &["rev-parse", "HEAD:"]);
    let (dir, file) = tree.split_at(2);
    std::fs::remove_file(app.join(".git/objects").join(dir).join(file)).unwrap();

    let e = ws.entry("app");
    let error = e.probe_error.as_ref().unwrap();
    assert_eq!(error.kind, ProbeErrorKind::GitFailed);
    assert!(error.message.contains("bad tree object HEAD"), "{e:?}");
    assert_eq!(
        e.layout.as_ref().map(|l| l.partial_filter.as_deref()),
        Some(None)
    );
    assert!(!e.probe_failed_partial());
}

#[test]
fn an_upstream_outside_a_narrowed_refspec_is_unmapped() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("wpt", &[]);
    ws.declare_repo("wpt", "wpt", "");
    ws.upstream_commit("wpt", "fork");
    // single-branch: the refspec maps only main
    let wpt = ws.clone_owned("wpt", "wpt", &["--single-branch", "--branch", "main"]);
    assert_eq!(
        ws.git(&wpt, &["config", "--get-all", "remote.origin.fetch"]),
        "+refs/heads/main:refs/remotes/origin/main"
    );
    // a work branch configured to track origin's `fork`
    ws.git(&wpt, &["branch", "fork"]);
    ws.git(&wpt, &["config", "branch.fork.remote", "origin"]);
    ws.git(&wpt, &["config", "branch.fork.merge", "refs/heads/fork"]);
    ws.git(&wpt, &["fetch", "-q", "origin"]);
    ws.assert_upstream(&wpt, "fork", "");
    assert!(!ws.has_ref(&wpt, "fork@{u}"));

    let e = ws.entry("wpt");
    assert!(e.needs_human.is_empty(), "{:?}", e.needs_human);
    let fork = branch(&e, "fork");
    assert_eq!(fork.relation, Relation::Unmapped);
    assert_eq!(fork.upstream.as_deref(), Some("origin/fork"));
    assert_eq!(
        fork.verdict,
        Verdict::NeedsHuman {
            reason: BranchNeedsHuman::Unmapped
        }
    );
    assert_eq!(branch(&e, "main").relation, Relation::InSync);
}
