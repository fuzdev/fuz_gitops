use super::*;

fn z(records: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in records {
        out.extend_from_slice(r.as_bytes());
        out.push(0);
    }
    out
}

#[test]
fn status_clean_on_branch() {
    let s = parse_status(&z(&[
        "# branch.oid 3890426260f93bbbdf34262c865872c38302836e",
        "# branch.head main",
        "# branch.upstream origin/main",
        "# stash 2",
    ]))
    .unwrap();
    assert_eq!(
        s.head,
        Head::Branch {
            name: "main".into()
        }
    );
    assert!(s.uncommitted.is_clean());
    assert_eq!(s.stashes, 2);
}

#[test]
fn status_detached() {
    let s = parse_status(&z(&["# branch.oid abc123", "# branch.head (detached)"])).unwrap();
    assert_eq!(
        s.head,
        Head::Detached {
            commit: "abc123".into()
        }
    );
}

#[test]
fn status_unborn() {
    let s = parse_status(&z(&["# branch.oid (initial)", "# branch.head main"])).unwrap();
    assert_eq!(
        s.head,
        Head::Branch {
            name: "main".into()
        }
    );
}

#[test]
fn status_splits_uncommitted() {
    let s = parse_status(&z(&[
        "# branch.oid abc",
        "# branch.head main",
        "1 .M N... 100644 100644 100644 aaa aaa package-lock.json",
        "1 M. N... 100644 100644 100644 aaa bbb staged.ts",
        "1 MM N... 100644 100644 100644 aaa bbb both.ts",
        "2 R. N... 100644 100644 100644 aaa aaa R100 new name.ts",
        "old name.ts",
        "u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.ts",
        "? untracked dir/",
        "? loose.txt",
    ]))
    .unwrap();
    assert_eq!(
        s.uncommitted,
        Uncommitted {
            staged: 3,
            unstaged: 2,
            untracked: 2,
            conflicted: 1
        }
    );
}

#[test]
fn status_rejects_garbage() {
    assert!(parse_status(&z(&["# branch.head main", "x what"])).is_err());
    assert!(parse_status(&z(&["# branch.oid abc"])).is_err());
}

#[test]
fn track_values() {
    assert_eq!(parse_track("").unwrap(), Track::Even);
    assert_eq!(parse_track("[gone]").unwrap(), Track::Gone);
    assert_eq!(parse_track("[ahead 3]").unwrap(), Track::Ahead(3));
    assert_eq!(parse_track("[behind 57]").unwrap(), Track::Behind(57));
    assert_eq!(
        parse_track("[ahead 1, behind 2]").unwrap(),
        Track::Diverged {
            ahead: 1,
            behind: 2
        }
    );
    assert!(parse_track("[sideways 1]").is_err());
    assert!(parse_track("ahead 1").is_err());
}

#[test]
fn refs_records() {
    let out = [
        "main\0c1\0\0refs/remotes/origin/main\0refs/heads/main\0[ahead 1]\0",
        "/home/me/dev/gro\0",
        "1759000000\n",
        "fork\0c2\0\0\0\0\0\0",
        "1758000000\n",
        "diff-rework\0c3\0\0refs/remotes/origin/diff-rework\0refs/heads/diff-rework\0",
        "[gone]\0\0",
        "1757000000\n",
        "m\0c1\0refs/heads/main\0\0\0\0\0",
        "1759000000\n",
    ]
    .concat();
    let refs = parse_refs(out.as_bytes()).unwrap();
    assert_eq!(refs.len(), 4);
    assert_eq!(
        refs[0],
        RefFacts {
            name: "main".into(),
            oid: "c1".into(),
            symref: None,
            upstream_ref: Some("refs/remotes/origin/main".into()),
            merge_ref: Some("refs/heads/main".into()),
            track: Track::Ahead(1),
            worktree: Some("/home/me/dev/gro".into()),
            committer_time: 1_759_000_000,
        }
    );
    assert_eq!(refs[1].upstream_ref, None);
    assert_eq!(refs[1].merge_ref, None);
    assert_eq!(refs[1].track, Track::Even);
    assert_eq!(refs[2].track, Track::Gone);
    assert_eq!(refs[3].symref.as_deref(), Some("refs/heads/main"));
    assert!(parse_refs(b"main\0only-two\n").is_err());
}

#[test]
fn worktree_records() {
    // what git prints: every attribute NUL-terminated, an empty one after
    // each record
    let out = z(&[
        "worktree /ws/app",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "branch refs/heads/main",
        "",
        "worktree /elsewhere/app-feat",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "branch refs/heads/feat/x",
        "",
        "worktree /ws/app-detached",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "detached",
        "",
        "worktree /ws/app-gone",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "branch refs/heads/gone",
        "prunable gitdir file points to non-existent location",
        "",
        "worktree /media/usb/app",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "branch refs/heads/usb",
        "locked on a\nremovable drive",
        "",
        "worktree /ws/app-locked",
        "HEAD 3890426260f93bbbdf34262c865872c38302836e",
        "detached",
        "locked",
        "some-future-attribute value",
        "",
    ]);
    let w = parse_worktrees(&out).unwrap();
    let paths: Vec<&str> = w.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/ws/app",
            "/elsewhere/app-feat",
            "/ws/app-detached",
            "/ws/app-gone",
            "/media/usb/app",
            "/ws/app-locked"
        ]
    );
    assert_eq!(
        w[1],
        WorktreeRecord {
            path: "/elsewhere/app-feat".into(),
            head: WorktreeHead::Branch {
                name: "feat/x".into()
            },
            locked: None,
            prunable: None,
        }
    );
    let oid = "3890426260f93bbbdf34262c865872c38302836e".to_owned();
    assert_eq!(
        w[2].head,
        WorktreeHead::Detached {
            commit: oid.clone()
        }
    );
    assert_eq!(
        w[3].prunable.as_deref(),
        Some("gitdir file points to non-existent location")
    );
    assert_eq!(w[3].locked, None);
    // under -z a reason keeps its newline verbatim
    assert_eq!(w[4].locked.as_deref(), Some("on a\nremovable drive"));
    assert_eq!(w[5].locked.as_deref(), Some(""));
    assert_eq!(w[5].head, WorktreeHead::Detached { commit: oid });
}

#[test]
fn worktree_records_nul_framing() {
    // a path with a space and a newline survives NUL framing
    let out = z(&[
        "worktree /repos/bare.git",
        "bare",
        "",
        "worktree /ws/odd name\nhere",
        "HEAD 0000000000000000000000000000000000000000",
        "branch refs/heads/unborn",
        "",
    ]);
    let w = parse_worktrees(&out).unwrap();
    assert_eq!(w.len(), 2);
    assert_eq!(w[0].head, WorktreeHead::Bare);
    assert_eq!(w[1].path, "/ws/odd name\nhere");
    assert_eq!(
        w[1].head,
        WorktreeHead::Branch {
            name: "unborn".into()
        }
    );
    // a final record without its empty terminator still counts
    let w = parse_worktrees(b"worktree /ws/app\0branch refs/heads/main\0").unwrap();
    assert_eq!(w.len(), 1);
    assert!(parse_worktrees(b"").unwrap().is_empty());
}

#[test]
fn worktree_heads_git_could_not_read_are_unknown() {
    // a missing HEAD lists as the null id and `detached`; a garbled one
    // as the null id alone; neither is a detached HEAD
    let out = z(&[
        "worktree /ws/missing-head",
        "HEAD 0000000000000000000000000000000000000000",
        "detached",
        "",
        "worktree /ws/garbled-head",
        "HEAD 0000000000000000000000000000000000000000",
        "",
        "worktree /ws/no-head-lines",
        "",
        "worktree /ws/not-an-id",
        "HEAD garbage",
        "detached",
        "",
        "worktree /ws/sha256",
        "HEAD 8a5b0f7f2bd8f0e1c3f1a0b9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9",
        "detached",
        "",
    ]);
    let heads: Vec<WorktreeHead> = parse_worktrees(&out)
        .unwrap()
        .into_iter()
        .map(|r| r.head)
        .collect();
    assert_eq!(
        heads[..4],
        [
            WorktreeHead::Unknown,
            WorktreeHead::Unknown,
            WorktreeHead::Unknown,
            WorktreeHead::Unknown
        ]
    );
    assert!(matches!(heads[4], WorktreeHead::Detached { .. }));
    assert!(is_object_id("3890426260f93bbbdf34262c865872c38302836e"));
    assert!(!is_object_id("0000000000000000000000000000000000000000"));
    assert!(!is_object_id("3890426260f93bbbdf34262c865872c3830283"));
    assert!(!is_object_id("ref: refs/heads/main"));
}

#[test]
fn worktree_records_reject_garbage() {
    // an attribute before any record
    assert!(parse_worktrees(b"detached\0\0").is_err());
    // two records run together without the empty separator
    assert!(parse_worktrees(b"worktree /a\0worktree /b\0\0").is_err());
    assert!(parse_worktrees(b"worktree /a\xff\0\0").is_err());
}

#[test]
fn gitlinks_from_the_index() {
    let out = z(&[
        "100644 78981922613b2afb6025042ff6bd878ac1994e85 0\ta",
        "160000 1e7973f7c6a50768ef95e46ddd24698efcf4c233 0\tnested repo",
        "160000 1e7973f7c6a50768ef95e46ddd24698efcf4c233 0\tdeps/sub",
        "120000 78981922613b2afb6025042ff6bd878ac1994e85 0\tlink",
    ]);
    assert_eq!(parse_gitlinks(&out).unwrap(), ["nested repo", "deps/sub"]);
    assert!(parse_gitlinks(&z(&["160000 no-tab"])).is_err());
    assert!(parse_gitlinks(b"").unwrap().is_empty());
}

#[test]
fn config_entries() {
    let out = local(&[
        "remote.origin.url\ngit@github.com:ryanatkn/wpt",
        "remote.origin.fetch\n+refs/heads/master:refs/remotes/origin/master",
        "remote.origin.promisor\ntrue",
        "remote.origin.partialclonefilter\nblob:none",
        "remote.upstream.url\nhttps://github.com/web-platform-tests/wpt",
        "branch.master.remote\norigin",
        "branch.master.merge\nrefs/heads/master",
        "branch.fork.remote\norigin",
        "branch.fork.merge\nrefs/heads/fork",
        "branch.feat.x.remote\nupstream",
        "branch.feat.x.merge\nrefs/heads/main",
        "branch.main.vscode-merge-base\norigin/main",
        "core.sparsecheckout\ntrue",
    ]);
    let c = parse(&out);
    assert_eq!(c.origin_url(), Some("git@github.com:ryanatkn/wpt"));
    assert_eq!(c.origin_keys, OriginKeys::InRepo);
    assert_eq!(
        c.origin_fetch,
        [ConfigValue::repo(
            "+refs/heads/master:refs/remotes/origin/master"
        )]
    );
    assert_eq!(c.partial_filter.as_deref(), Some("blob:none"));
    assert!(c.sparse && !c.ssh_command);
    assert!(c.branches["fork"].is_origin());
    assert_eq!(c.branches["fork"].display().as_deref(), Some("origin/fork"));
    assert!(!c.branches["feat.x"].is_origin());
    assert_eq!(
        c.branches["feat.x"].display().as_deref(),
        Some("upstream/main")
    );
    assert!(!c.branches.contains_key("main"));
    // origin, the only promisor: a lazy fetch reaches nothing else
    assert!(!c.other_promisor);
}

#[test]
fn config_promisors_besides_origin() {
    let promisor = |entries: &[&str]| parse(&local(entries)).other_promisor;
    let origin = [
        "remote.origin.promisor\ntrue",
        "extensions.partialclone\norigin",
    ];
    assert!(!promisor(&origin));
    assert!(!promisor(&["remote.up.promisor\nfalse"]));
    for other in [
        "remote.up.promisor\ntrue",
        // valueless: true
        "remote.up.promisor",
        "extensions.partialclone\nup",
    ] {
        assert!(promisor(&[origin[0], other]), "{other}");
    }
    // one true value keeps it, whatever git reads last
    assert!(promisor(&[
        "remote.up.promisor\ntrue",
        "remote.up.promisor\nfalse"
    ]));
}

#[test]
fn config_valueless_boolean_and_ssh() {
    let c = parse(&local(&[
        "core.sparsecheckout",
        "core.sshcommand\nssh -i key",
    ]));
    assert!(c.sparse && c.ssh_command);
    let c = parse(&local(&["core.sparsecheckout\nfalse"]));
    assert!(!c.sparse);
    assert_eq!(parse(b""), ConfigFacts::default());
}

/// Entries as `--show-scope --show-origin -z` prints them.
fn scoped(entries: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (scope, origin, entry) in entries {
        for field in [scope, origin, entry] {
            out.extend_from_slice(field.as_bytes());
            out.push(0);
        }
    }
    out
}

/// Entries from the repo's own config file.
fn local(entries: &[&str]) -> Vec<u8> {
    let entries: Vec<_> = entries
        .iter()
        .map(|e| ("local", "file:.git/config", *e))
        .collect();
    scoped(&entries)
}

fn parse(out: &[u8]) -> ConfigFacts {
    ConfigFacts::parse(out, |path| path == ".git/config").unwrap()
}

#[test]
fn config_scopes_and_the_url_list() {
    // bytes as git 2.47 printed them, the include's path absolute
    let c = parse(&scoped(&[
        (
            "global",
            "file:/home/me/.gitconfig",
            "remote.origin.fetch\n+refs/pull/*/head:refs/remotes/origin/pr/*",
        ),
        (
            "local",
            "file:.git/config",
            "remote.origin.url\nhttps://me:tok@github.com/old/a",
        ),
        (
            "local",
            "file:.git/config",
            "remote.origin.url\ngit@github.com:me/mirror",
        ),
        (
            "local",
            "file:/srv/inc.cfg",
            "remote.origin.url\nfile:///inc",
        ),
        (
            "local",
            "file:.git/config",
            "remote.origin.fetch\n+refs/heads/main:refs/remotes/origin/main",
        ),
    ]));
    // the first URL wins, never the last
    assert_eq!(c.origin_url(), Some("https://me:tok@github.com/old/a"));
    assert_eq!(
        c.origin_urls,
        [
            OriginUrl::repo("https://me:tok@github.com/old/a"),
            OriginUrl::repo("git@github.com:me/mirror"),
            OriginUrl::elsewhere("file:///inc"),
        ]
    );
    assert_eq!(
        c.origin_fetch,
        [
            ConfigValue::elsewhere("+refs/pull/*/head:refs/remotes/origin/pr/*"),
            ConfigValue::repo("+refs/heads/main:refs/remotes/origin/main"),
        ]
    );
    // a valueless `url` is flagged, and reads as empty
    let c = parse(&local(&["remote.origin.url"]));
    assert_eq!(c.origin_urls, [OriginUrl::valueless()]);
    assert_eq!(c.origin_url(), None);
    let c = parse(&local(&["remote.origin.url\n"]));
    assert_eq!(c.origin_urls, [OriginUrl::repo("")]);
    // an empty value resets the list; a later one starts it again
    let c = parse(&local(&["remote.origin.url\nx", "remote.origin.url\n"]));
    assert_eq!(c.origin_url(), None);
    assert!(c.origin_url_list().is_empty() && c.origin_urls.len() == 2);
    let c = parse(&local(&["remote.origin.url\n", "remote.origin.url\ny"]));
    assert_eq!(c.origin_url(), Some("y"));
    // global and system keys aren't the repo's; worktree keys are, but
    // not its file
    let c = parse(&scoped(&[("global", "file:/g", "remote.origin.url\nx")]));
    assert_eq!(c.origin_keys, OriginKeys::Elsewhere);
    let c = parse(&scoped(&[(
        "worktree",
        "file:.git/config.worktree",
        "remote.origin.url\nx",
    )]));
    assert_eq!(c.origin_keys, OriginKeys::InRepo);
    assert_eq!(c.origin_urls, [OriginUrl::elsewhere("x")]);
    // an in-repo key stays in-repo whatever scope comes after it
    let c = parse(&scoped(&[
        ("local", "file:.git/config", "remote.origin.fetch\nx"),
        ("command", "command line:", "remote.origin.url\ny"),
    ]));
    assert_eq!(c.origin_keys, OriginKeys::InRepo);
    // only exactly `origin` is origin: a remote named `origin/fork` or
    // `origin.old` is another remote, its refspecs collected
    let c = parse(&local(&[
        "remote.origin/fork.url\nfile:///fork",
        "remote.origin/fork.fetch\n+refs/heads/*:refs/remotes/origin/fork/*",
        "remote.origin.old.fetch\n+refs/heads/*:refs/remotes/old/*",
    ]));
    assert_eq!(c.origin_keys, OriginKeys::None);
    assert!(c.origin_urls.is_empty() && c.origin_fetch.is_empty());
    assert_eq!(
        c.other_fetch,
        [
            RemoteRefspec {
                remote: "origin/fork".into(),
                refspec: "+refs/heads/*:refs/remotes/origin/fork/*".into(),
            },
            RemoteRefspec {
                remote: "origin.old".into(),
                refspec: "+refs/heads/*:refs/remotes/old/*".into(),
            },
        ]
    );
    // a torn entry fails loud
    assert!(ConfigFacts::parse(b"local\0file:.git/config\0", |_| true).is_err());
    // an origin path that isn't UTF-8 parses on, and is never the repo's
    // own file — even to a predicate that would match anything
    let c = ConfigFacts::parse(
        b"global\0file:/home/me/g\xff.gitconfig\0branch.main.remote\norigin\0\
          local\0file:/home/me/\xfe/inc\0remote.origin.url\ngit@github.com:old/app\0",
        |_| true,
    )
    .unwrap();
    assert_eq!(c.branches["main"].remote.as_deref(), Some("origin"));
    assert_eq!(
        c.origin_urls,
        [OriginUrl::elsewhere("git@github.com:old/app")]
    );
    // a key or value that isn't UTF-8 still fails loud
    assert!(
        ConfigFacts::parse(
            b"local\0file:.git/config\0remote.origin.url\n\xff\0",
            |_| true
        )
        .is_err()
    );
}
