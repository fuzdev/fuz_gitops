//! What the fetch may write: remote-tracking refs, nothing else.

mod support;

use std::path::Path;

use fuz_repos::remote::RemoteFailure;
use fuz_repos::state::Relation;
use support::remote::drift;
use support::{FixtureWorkspace, branch, find_entry};

/// Whether plain `git fetch --prune origin` in `repo`, as a person would run
/// it, succeeds — the control each guard below is measured against.
fn plain_fetch(ws: &FixtureWorkspace, repo: &Path) -> bool {
    ws.git_output(
        repo,
        &[
            "-c",
            "maintenance.auto=false",
            "fetch",
            "-q",
            "--prune",
            "origin",
        ],
    )
    .status
    .success()
}

#[test]
fn the_fetch_never_prunes_or_follows_tags() {
    let mut ws = FixtureWorkspace::new();
    for name in ["prune", "rprune", "follow", "tagopt", "control"] {
        ws.owned_repo(name, &[]);
    }
    for (name, key) in [
        ("prune", "fetch.pruneTags"),
        ("rprune", "remote.origin.pruneTags"),
        ("control", "fetch.pruneTags"),
    ] {
        let repo = ws.dir(name);
        ws.git(&repo, &["tag", "v9-local-only"]);
        ws.git(&repo, &["config", key, "true"]);
    }
    ws.git(
        &ws.dir("tagopt"),
        &["config", "remote.origin.tagOpt", "--tags"],
    );
    // a new commit and tag upstream for every repo
    for name in ["prune", "rprune", "follow", "tagopt", "control"] {
        ws.upstream_commit(name, "main");
        let up = ws.upstream(name);
        ws.git(&up, &["tag", "v2"]);
        ws.git(&up, &["push", "-q", "origin", "v2"]);
    }
    // control: a plain fetch deletes the unpushed tag and follows the new one
    let control = ws.dir("control");
    assert!(plain_fetch(&ws, &control));
    assert_eq!(ws.git(&control, &["tag"]), "v2");

    let entries = ws.status_with_fetch();
    for (name, tags) in [
        ("prune", "v9-local-only"),
        ("rprune", "v9-local-only"),
        ("follow", ""),
        ("tagopt", ""),
    ] {
        let e = find_entry(&entries, name);
        assert_eq!(e.fetch_error, None, "{name}");
        // the fetch ran: the branch reads behind
        assert_eq!(
            branch(e, "main").relation,
            Relation::Behind { commits: 1 },
            "{name}"
        );
        assert_eq!(ws.git(&ws.dir(name), &["tag"]), tags, "{name}");
    }
}

#[test]
fn the_fetch_never_recurses_into_submodules() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("sub", &[]);
    ws.remote("app", &[]);
    let up = ws.upstream("app");
    let sub_url = format!("file://{}", ws.bare("sub").display());
    ws.git(
        &up,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub_url,
            "sub",
        ],
    );
    ws.git(&up, &["commit", "-q", "-m", "add sub"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    ws.declare_repo("app", "app", "");
    let app = ws.clone_owned("app", "app", &[]);
    // a second clone for the control, never registered: once a fetch has
    // brought the new commits, a later one has nothing to recurse for
    let control = ws.clone_owned("control", "app", &[]);
    for repo in [&app, &control] {
        ws.git(
            repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "update",
                "-q",
                "--init",
            ],
        );
    }
    let modules = app.join(".git/modules/sub");
    assert!(modules.join("HEAD").is_file());
    // the submodule moves upstream, the parent's pointer with it, and then
    // the submodule's URL goes away
    ws.upstream_commit("sub", "main");
    ws.git(&up.join("sub"), &["pull", "-q", "origin", "main"]);
    ws.git(&up, &["commit", "-q", "-am", "bump sub"]);
    ws.git(&up, &["push", "-q", "origin", "main"]);
    std::fs::rename(ws.bare("sub"), ws.outside("sub-moved.git")).unwrap();
    // control: a plain fetch recurses, and fails for the submodule
    let out = ws.git_output(
        &control,
        &[
            "-c",
            "maintenance.auto=false",
            "fetch",
            "-q",
            "--prune",
            "origin",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Errors during submodule fetch"));
    let before = support::snapshot_git_dir(&modules);

    let e = support::take_entry(ws.status_with_fetch(), "app");
    // the entry's own fetch ran, and is all that ran
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::Behind { commits: 1 });
    support::assert_git_dir_unchanged(&before, &support::snapshot_git_dir(&modules));
}

#[test]
fn the_fetch_writes_no_commit_graph_and_no_bundles() {
    let mut ws = FixtureWorkspace::new();
    for name in ["graph", "graph_control", "bundle", "bundle_control"] {
        ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
    }
    for name in ["graph", "graph_control"] {
        ws.git(&ws.dir(name), &["config", "fetch.writeCommitGraph", "true"]);
    }
    for name in ["bundle", "bundle_control"] {
        let bundle = ws.outside(&format!("{name}.bundle"));
        ws.git(
            &ws.upstream(name),
            &["bundle", "create", "-q", bundle.to_str().unwrap(), "main"],
        );
        let repo = ws.dir(name);
        let url = format!("file://{}", bundle.display());
        ws.git(&repo, &["config", "fetch.bundleURI", &url]);
    }
    let graph = |name: &str| {
        let info = ws.dir(name).join(".git/objects/info");
        ["commit-graph", "commit-graphs"]
            .iter()
            .any(|f| info.join(f).exists())
    };
    let bundles = |name: &str| {
        ws.git(&ws.dir(name), &["for-each-ref", "refs/bundles"])
            .lines()
            .count()
    };
    assert!(!graph("graph") && bundles("bundle") == 0);
    // controls: a plain fetch writes both
    assert!(plain_fetch(&ws, &ws.dir("graph_control")));
    assert!(graph("graph_control"));
    assert!(plain_fetch(&ws, &ws.dir("bundle_control")));
    assert!(bundles("bundle_control") > 0);

    let entries = ws.status_with_fetch();
    for name in ["graph", "bundle"] {
        let e = find_entry(&entries, name);
        assert_eq!(e.fetch_error, None, "{name}");
        assert_eq!(
            branch(e, "main").relation,
            Relation::Behind { commits: 1 },
            "{name}"
        );
    }
    assert!(!graph("graph"));
    assert_eq!(bundles("bundle"), 0);
}

#[test]
fn a_refspec_writing_outside_remote_tracking_refs_is_not_fetched() {
    let mut ws = FixtureWorkspace::new();
    for name in ["tags", "mirror"] {
        ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        let up = ws.upstream(name);
        ws.git(&up, &["tag", "v2"]);
        ws.git(&up, &["push", "-q", "origin", "v2"]);
    }
    let tags = ws.dir("tags");
    ws.git(
        &tags,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    );
    let mirror = ws.dir("mirror");
    ws.git(&mirror, &["checkout", "-q", "--detach"]);
    ws.git(
        &mirror,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ],
    );
    let main_before = ws.git(&mirror, &["rev-parse", "main"]);

    let entries = ws.status_with_fetch();
    for (name, refspec) in [
        ("tags", "+refs/tags/*:refs/tags/*"),
        ("mirror", "+refs/heads/*:refs/heads/*"),
    ] {
        let e = find_entry(&entries, name);
        assert_eq!(
            e.fetch_error,
            Some(RemoteFailure::RefspecOutsideOrigin {
                refspec: refspec.into()
            }),
            "{name}"
        );
        assert!(!ws.dir(name).join(".git/FETCH_HEAD").exists(), "{name}");
    }
    assert_eq!(ws.git(&tags, &["tag"]), "");
    assert_eq!(ws.git(&mirror, &["rev-parse", "main"]), main_before);
    // control: a plain fetch writes both
    assert!(plain_fetch(&ws, &tags));
    assert_eq!(ws.git(&tags, &["tag"]), "v2");
    assert!(plain_fetch(&ws, &mirror));
    assert_ne!(ws.git(&mirror, &["rev-parse", "main"]), main_before);
}

#[test]
fn a_refspec_writing_into_another_remotes_refs_is_not_fetched() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("other", &[]);
    ws.upstream_commit("other", "only-upstream");
    let other_url = format!("file://{}", ws.bare("other").display());
    let cases = [
        ("into_upstream", "+refs/heads/*:refs/remotes/upstream/*"),
        ("into_all", "+refs/heads/*:refs/remotes/*"),
    ];
    for (name, refspec) in cases {
        let repo = ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        ws.git(&repo, &["remote", "add", "upstream", &other_url]);
        ws.git(&repo, &["fetch", "-q", "upstream"]);
        ws.git(&repo, &["config", "--add", "remote.origin.fetch", refspec]);
    }
    let remote_refs = |repo: &Path| ws.git(repo, &["for-each-ref", "refs/remotes"]);
    let before: Vec<String> = cases.iter().map(|(n, _)| remote_refs(&ws.dir(n))).collect();
    for b in &before {
        assert!(b.contains("refs/remotes/upstream/only-upstream"), "{b}");
    }

    let entries = ws.status_with_fetch();
    for ((name, refspec), before) in cases.iter().zip(&before) {
        assert_eq!(
            find_entry(&entries, name).fetch_error,
            Some(RemoteFailure::RefspecOutsideOrigin {
                refspec: (*refspec).into()
            }),
            "{name}"
        );
        // another remote's tracking refs untouched
        assert_eq!(&remote_refs(&ws.dir(name)), before, "{name}");
    }
    // control: a plain fetch clobbers them, pruning `upstream`'s own branch
    for (name, _) in cases {
        let repo = ws.dir(name);
        assert!(plain_fetch(&ws, &repo));
        assert!(
            !remote_refs(&repo).contains("refs/remotes/upstream/only-upstream"),
            "{name}"
        );
    }
}

#[test]
fn a_remote_nested_under_origin_is_not_pruned_away() {
    let mut ws = FixtureWorkspace::new();
    ws.remote("fork", &[]);
    ws.upstream_commit("fork", "feat");
    let fork_url = format!("file://{}", ws.bare("fork").display());
    for name in ["app", "control"] {
        let repo = ws.owned_repo(name, &[]);
        ws.upstream_commit(name, "main");
        // a remote named `origin/fork`: its refs land under origin's
        ws.git(&repo, &["remote", "add", "origin/fork", &fork_url]);
        ws.git(&repo, &["fetch", "-q", "origin/fork"]);
    }
    let nested = |repo: &Path| {
        ws.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/remotes/origin/fork",
            ],
        )
    };
    let app = ws.dir("app");
    let before = nested(&app);
    assert_eq!(
        before,
        "refs/remotes/origin/fork/feat\nrefs/remotes/origin/fork/main"
    );
    // control: a plain pruning fetch of origin deletes them
    let control = ws.dir("control");
    assert!(plain_fetch(&ws, &control));
    assert_eq!(nested(&control), "");

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(
        e.fetch_error,
        Some(RemoteFailure::OriginRefsShared {
            remote: "origin/fork".into(),
            refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
        })
    );
    assert_eq!(nested(&app), before);
    // origin's own view isn't the other remote's: no drift
    assert!(drift(&e).is_none());
}

#[test]
fn a_glob_or_legacy_remote_under_origin_is_not_pruned_away() {
    let mut ws = FixtureWorkspace::new();
    // a fork with branches whose names a `*` carries under origin's, and a
    // tracking ref of its own at `refs/remotes/origin/z`
    ws.remote("fork", &[]);
    ws.upstream_commit("fork", "fx");
    ws.upstream_commit("fork", "origin/y");
    let up = ws.upstream("fork");
    ws.git(&up, &["update-ref", "refs/remotes/origin/z", "HEAD"]);
    ws.git(&up, &["push", "-q", "origin", "refs/remotes/origin/z"]);
    let fork_url = format!("file://{}", ws.bare("fork").display());
    // (entry, refspec, legacy `Pull:` line rather than config, the ref it
    // shares with origin)
    let cases = [
        (
            "glob_origin",
            "+refs/heads*:refs/remotes/origin*",
            false,
            "refs/remotes/origin/fx",
        ),
        (
            "glob_remotes",
            "+refs/heads/*:refs/remotes/*",
            false,
            "refs/remotes/origin/y",
        ),
        ("glob_refs", "+refs*:refs*", false, "refs/remotes/origin/z"),
        ("glob_ref", "+ref*:ref*", false, "refs/remotes/origin/z"),
        (
            "legacy",
            "+refs/heads/fx:refs/remotes/origin/fork-fx",
            true,
            "refs/remotes/origin/fork-fx",
        ),
        ("legacy_glob", "+ref*:ref*", true, "refs/remotes/origin/z"),
    ];
    for (name, refspec, legacy, _) in cases {
        for dir in [name.to_owned(), format!("{name}_control")] {
            let repo = if dir == name {
                ws.owned_repo(name, &[])
            } else {
                ws.clone_owned(&dir, name, &[])
            };
            // a full-name glob would fetch into the checked-out branch
            ws.git(&repo, &["checkout", "-q", "--detach"]);
            let remote = if legacy {
                support::write(
                    &repo,
                    ".git/remotes/legacy",
                    &format!("URL: {fork_url}\nPull: {refspec}\n"),
                );
                "legacy"
            } else {
                ws.git(&repo, &["remote", "add", "fork", &fork_url]);
                ws.git(
                    &repo,
                    &["config", "--replace-all", "remote.fork.fetch", refspec],
                );
                "fork"
            };
            ws.git(&repo, &["fetch", "-q", remote]);
        }
    }
    let has = |repo: &Path, r: &str| ws.has_ref(repo, r);
    for (name, _, _, shared) in cases {
        assert!(has(&ws.dir(name), shared), "{name}");
        // control: a plain pruning fetch of origin deletes it
        let control = ws.dir(&format!("{name}_control"));
        assert!(has(&control, shared), "{name}");
        assert!(plain_fetch(&ws, &control));
        assert!(!has(&control, shared), "control {name}");
    }

    let entries = ws.status_with_fetch();
    for (name, refspec, legacy, shared) in cases {
        let e = find_entry(&entries, name);
        let remote = if legacy { "legacy" } else { "fork" };
        assert_eq!(
            e.fetch_error,
            Some(RemoteFailure::OriginRefsShared {
                remote: remote.into(),
                refspec: refspec.into(),
            }),
            "{name}"
        );
        assert!(has(&ws.dir(name), shared), "{name}");
    }
}

#[test]
fn a_legacy_origin_file_beside_a_configured_origin_is_ignored() {
    let mut ws = FixtureWorkspace::new();
    let app = ws.owned_repo("app", &[]);
    ws.upstream_commit("app", "main");
    // git reads `remotes/origin` only when config gives origin no URL
    support::write(
        &app,
        ".git/remotes/origin",
        "URL: file:///nowhere\nPull: +refs/heads/*:refs/remotes/origin/*\n",
    );
    // control: git ignores it (its URL goes nowhere, and the fetch works)
    assert!(plain_fetch(&ws, &app));
    ws.upstream_commit("app", "main");

    let e = support::take_entry(ws.status_with_fetch(), "app");
    assert_eq!(e.fetch_error, None);
    assert_eq!(branch(&e, "main").relation, Relation::Behind { commits: 2 });
}
