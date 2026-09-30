//! How worktree gitfiles, HEADs, and git dirs are read, as git reads them.

mod support;

use std::path::Path;

use fuz_repos::state::{UnprobedHead, UnprobedWhy};
use support::worktrees::app;
use support::{FixtureWorkspace, path};

#[test]
fn a_listed_worktrees_gitfile_is_read_as_git_reads_it() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let add = |name: &str| {
        let wt = ws.dir(name);
        let git_dir = ws.add_worktree(&app, &wt, &["-b", name]);
        (wt, git_dir)
    };
    // each `.git` rewritten to name its own git dir: git follows a path cut
    // at a NUL, and one that isn't UTF-8 (a link to the git dir)
    let (nul, nul_git_dir) = add("nul");
    let gitfile = |named: &[u8], tail: &[u8]| [b"gitdir: ", named, tail].concat();
    let nul_gitfile = gitfile(nul_git_dir.as_os_str().as_bytes(), b"\0junk\n");
    std::fs::write(nul.join(".git"), nul_gitfile).unwrap();
    let (raw, raw_git_dir) = add("raw");
    let link = ws.base().join(std::ffi::OsStr::from_bytes(b"admin-\xff"));
    std::os::unix::fs::symlink(&raw_git_dir, &link).unwrap();
    let raw_gitfile = gitfile(link.as_os_str().as_bytes(), b"\n");
    std::fs::write(raw.join(".git"), raw_gitfile).unwrap();
    ws.assert_head(&nul, Some("nul"));
    ws.assert_head(&raw, Some("raw"));
    // and refuses its `gitdir: ` on a second line, or with a trailing space
    let (second, second_git_dir) = add("second");
    let second_gitfile = [
        b"x\n".as_slice(),
        &gitfile(second_git_dir.as_os_str().as_bytes(), b"\n"),
    ]
    .concat();
    std::fs::write(second.join(".git"), second_gitfile).unwrap();
    let (spaced, spaced_git_dir) = add("spaced");
    let spaced_gitfile = gitfile(spaced_git_dir.as_os_str().as_bytes(), b" \n");
    std::fs::write(spaced.join(".git"), spaced_gitfile).unwrap();
    ws.git_fails(&second, &["status"]);
    ws.git_fails(&spaced, &["status"]);
    // git lists all four
    for wt in [&nul, &raw, &second, &spaced] {
        ws.worktree_record(&app, wt);
    }

    let e = ws.entry("app");
    let mut probed: Vec<&str> = e
        .checkouts
        .iter()
        .filter(|c| c.linked)
        .map(|c| c.path.as_str())
        .collect();
    probed.sort_unstable();
    assert_eq!(probed, [path(&nul), path(&raw)]);
    let mut failed: Vec<(&str, &str)> = e
        .unprobed_worktrees
        .iter()
        .map(|u| match &u.worktree.why {
            UnprobedWhy::Failed { error } => (u.worktree.path.as_str(), error.as_str()),
            why => panic!("{}: {why:?}", u.worktree.path),
        })
        .collect();
    failed.sort_unstable();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert_eq!(failed[0].0, path(&second));
    let invalid = format!("invalid gitfile format: {}", second.join(".git").display());
    assert_eq!(failed[0].1, invalid);
    assert_eq!(failed[1].0, path(&spaced));
    assert!(failed[1].1.contains("can't be resolved"), "{}", failed[1].1);
}

#[test]
fn an_unlisted_worktrees_head_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let commit = ws.git(&app, &["rev-parse", "main"]);
    let heads: [(&str, Vec<u8>); 4] = [
        ("nospace", b"ref:refs/heads/nospace\n".to_vec()),
        ("junk", format!("{commit} junk\n").into_bytes()),
        ("lead", b" ref: refs/heads/lead\n".to_vec()),
        ("nul", b"ref: refs/heads/nul\0junk\n".to_vec()),
    ];
    for (name, head) in &heads {
        let git_dir = ws.add_worktree(&app, &ws.dir(name), &["-b", name]);
        std::fs::write(git_dir.join("HEAD"), head).unwrap();
        // git lists no worktree whose git dir names none
        std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    }
    let list = ws.git(&app, &["worktree", "list", "--porcelain"]);
    for (name, _) in &heads {
        assert!(!list.contains(&path(&ws.dir(name))), "{list}");
    }
    // git, working through each: on a branch, detached, or no repo at all
    let symref = |name: &str| ws.git(&ws.dir(name), &["symbolic-ref", "HEAD"]);
    assert_eq!(symref("nospace"), "refs/heads/nospace");
    assert_eq!(symref("nul"), "refs/heads/nul");
    assert_eq!(ws.git(&ws.dir("junk"), &["rev-parse", "HEAD"]), commit);
    ws.git_fails(&ws.dir("junk"), &["symbolic-ref", "-q", "HEAD"]);
    ws.git_fails(&ws.dir("lead"), &["rev-parse", "HEAD"]);

    let e = ws.entry("app");
    let head_of = |name: &str| {
        let suffix = format!("/worktrees/{name}");
        let found: Vec<&UnprobedHead> = e
            .unprobed_worktrees
            .iter()
            .filter(|u| u.worktree.path.ends_with(&suffix))
            .map(|u| &u.worktree.head)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", e.unprobed_worktrees);
        found[0].clone()
    };
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    assert_eq!(head_of("nospace"), on("nospace"));
    assert_eq!(head_of("nul"), on("nul"));
    assert_eq!(head_of("junk"), UnprobedHead::Detached { commit });
    assert_eq!(head_of("lead"), UnprobedHead::Unknown);
}

#[test]
fn an_unlisted_worktrees_symlinked_head_is_read_as_git_reads_it() {
    let mut ws = FixtureWorkspace::new();
    let app = app(&mut ws);
    let commit = ws.git(&app, &["rev-parse", "main"]);
    ws.git(&app, &["tag", "v1"]);
    // git itself writes a symlink under `core.preferSymlinkRefs`
    let sym = ws.dir("sym");
    ws.git(
        &app,
        &[
            "-c",
            "core.preferSymlinkRefs=true",
            "worktree",
            "add",
            "-q",
            sym.to_str().unwrap(),
            "-b",
            "sym",
        ],
    );
    let git_dir = |name: &str| app.join(".git/worktrees").join(name);
    let link = std::fs::read_link(git_dir("sym").join("HEAD")).unwrap();
    assert_eq!(link, Path::new("refs/heads/sym"));
    // the rest by hand: a tag, a loose ref read through, and an invalid ref
    // name git falls through to reading as a file
    let relink = |name: &str, to: &str| {
        let head = git_dir(name).join("HEAD");
        std::fs::remove_file(&head).unwrap();
        std::os::unix::fs::symlink(to, &head).unwrap();
    };
    for name in ["tag", "through", "fall"] {
        ws.add_worktree(&app, &ws.dir(name), &["-b", name]);
    }
    relink("tag", "refs/tags/v1");
    relink("through", "../../refs/heads/through");
    std::fs::create_dir_all(git_dir("fall").join("refs/heads")).unwrap();
    std::fs::write(
        git_dir("fall").join("refs/heads/x y"),
        "ref: refs/heads/fall\n",
    )
    .unwrap();
    relink("fall", "refs/heads/x y");
    // git's view of each
    let record = |name: &str| ws.worktree_record(&app, &ws.dir(name))[1..].to_vec();
    assert_eq!(
        record("sym"),
        [format!("HEAD {commit}"), "branch refs/heads/sym".into()]
    );
    assert_eq!(
        record("tag"),
        [format!("HEAD {commit}"), "branch refs/tags/v1".into()]
    );
    assert_eq!(
        record("through"),
        [format!("HEAD {commit}"), "detached".into()]
    );
    assert_eq!(
        record("fall"),
        [format!("HEAD {commit}"), "branch refs/heads/fall".into()]
    );
    // unlisted, so the tool reads each `HEAD` itself
    for name in ["sym", "tag", "through", "fall"] {
        std::fs::remove_file(git_dir(name).join("gitdir")).unwrap();
    }

    let e = ws.entry("app");
    let head_of = |name: &str| {
        let suffix = format!("/worktrees/{name}");
        let found: Vec<&UnprobedHead> = e
            .unprobed_worktrees
            .iter()
            .filter(|u| u.worktree.path.ends_with(&suffix))
            .map(|u| &u.worktree.head)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", e.unprobed_worktrees);
        found[0].clone()
    };
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    assert_eq!(head_of("sym"), on("sym"));
    // the tool reports only branches
    assert_eq!(head_of("tag"), UnprobedHead::Unknown);
    assert_eq!(head_of("through"), UnprobedHead::Detached { commit });
    assert_eq!(head_of("fall"), on("fall"));
}
