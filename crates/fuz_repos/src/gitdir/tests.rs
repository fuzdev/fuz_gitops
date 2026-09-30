use super::*;

/// What a gitfile names, relative to its dir unless absolute, or how
/// git refuses it.
#[derive(Debug)]
enum Gitfile {
    Names(&'static [u8]),
    Invalid,
    NoPath,
}

#[test]
fn a_gitfile_is_read_as_git_reads_it() {
    use Gitfile::{Invalid, Names, NoPath};
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let dot_git = base.join(".git");
    // each row probed against git 2.47 (`git rev-parse --git-dir` in a
    // linked worktree whose `.git` holds the bytes)
    let rows: [(&[u8], Gitfile); 17] = [
        // git: follows it, relative to the file's dir or absolute
        (b"gitdir: ../g\n", Names(b"../g")),
        (b"gitdir: /g/w", Names(b"/g/w")),
        // git: trims trailing line breaks alone, CR included
        (b"gitdir: /g/w\r\n", Names(b"/g/w")),
        (b"gitdir: g\n\n\r\n", Names(b"g")),
        // git: keeps a trailing space or tab (then "not a git repository")
        (b"gitdir: g \n", Names(b"g ")),
        (b"gitdir: g\t\n", Names(b"g\t")),
        // git: a second line is part of the path (then "not a git
        // repository")
        (b"gitdir: g\nextra\n", Names(b"g\nextra")),
        // git: a C string, cut at the first NUL after the end is trimmed
        (b"gitdir: /g\0junk\n", Names(b"/g")),
        (b"gitdir: /g\n\0\n", Names(b"/g\n")),
        // nothing before the NUL: the file's own dir, as git joins it
        (b"gitdir: \0x", Names(b"")),
        // git: raw bytes, UTF-8 or not
        (b"gitdir: /g\xff\n", Names(b"/g\xff")),
        // git: "invalid gitfile format" — the prefix exactly, first
        (b"gitdir:g\n", Invalid),
        (b" gitdir: g\n", Invalid),
        (b"x\ngitdir: g\n", Invalid),
        (b"gitdir\0: g", Invalid),
        // git: "no path in gitfile"
        (b"gitdir: \n", NoPath),
        // git: an empty one too
        (b"", Invalid),
    ];
    for (content, want) in rows {
        std::fs::write(&dot_git, content).unwrap();
        let got = read_gitfile(&dot_git);
        let ok = match (&got, &want) {
            (Ok(path), Names(named)) => *path == base.join(OsStr::from_bytes(named)),
            (Err(GitfileError::InvalidFormat { .. }), Invalid)
            | (Err(GitfileError::NoPath { .. }), NoPath) => true,
            _ => false,
        };
        assert!(ok, "{content:?}: {got:?}, want {want:?}");
    }
    // git's size limit, padding included: "too large to be a .git file"
    let max = usize::try_from(MAX_GITFILE_BYTES).unwrap();
    let mut at_limit = b"gitdir: /g".to_vec();
    at_limit.resize(max, b'\n');
    std::fs::write(&dot_git, &at_limit).unwrap();
    assert_eq!(read_gitfile(&dot_git).unwrap(), Path::new("/g"));
    at_limit.push(b'\n');
    std::fs::write(&dot_git, &at_limit).unwrap();
    assert!(matches!(
        read_gitfile(&dot_git),
        Err(GitfileError::Unreadable { .. })
    ));
    // git: not a regular file, not a gitfile (it's tried as a dir)
    std::fs::remove_file(&dot_git).unwrap();
    std::fs::create_dir(&dot_git).unwrap();
    assert!(matches!(
        read_gitfile(&dot_git),
        Err(GitfileError::Unreadable { .. })
    ));
}

#[test]
fn a_dot_git_names_itself_or_what_its_gitfile_names() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let dot_git = base.join(".git");
    std::fs::create_dir(&dot_git).unwrap();
    assert_eq!(dot_git_target(&dot_git).unwrap(), dot_git);
    // a link to a dir is the dir, as git follows it
    let link = base.join("link");
    std::os::unix::fs::symlink(&dot_git, &link).unwrap();
    assert_eq!(dot_git_target(&link).unwrap(), link);
    std::fs::remove_dir(&dot_git).unwrap();
    let why = |content: &[u8]| {
        std::fs::write(&dot_git, content).unwrap();
        dot_git_target(&dot_git)
    };
    assert_eq!(why(b"gitdir: g\n").unwrap(), base.join("g"));
    let shown = dot_git.display();
    assert_eq!(
        why(b"x\ngitdir: g\n").unwrap_err(),
        format!("invalid gitfile format: {shown}")
    );
    assert_eq!(
        why(b"gitdir: \r\n").unwrap_err(),
        format!("no path in gitfile: {shown}")
    );
    std::fs::remove_file(&dot_git).unwrap();
    let missing = dot_git_target(&dot_git).unwrap_err();
    assert!(
        missing.starts_with(&format!("reading {shown}: ")),
        "{missing}"
    );
}

#[test]
fn a_symlinked_head_is_read_as_git_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path();
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    let id = "0123456789abcdef0123456789abcdef01234567";
    let at = || UnprobedHead::Detached { commit: id.into() };
    let unknown = || UnprobedHead::Unknown;
    std::fs::write(git_dir.join("loose"), format!("{id}\n")).unwrap();
    std::fs::write(git_dir.join("symref"), "ref: refs/heads/through\n").unwrap();
    std::fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
    std::fs::write(git_dir.join("refs/heads/x y"), "ref: refs/heads/main\n").unwrap();
    let abs_symref = git_dir.join("symref");
    // each row probed against git 2.47 (`git worktree list` in a repo
    // whose linked worktree's `HEAD` is a link with the text)
    let rows: Vec<(&[u8], UnprobedHead)> = vec![
        // git: a link text that starts `refs/` and is a valid ref name
        // names that ref, unfollowed (as `core.preferSymlinkRefs` writes)
        (b"refs/heads/w", on("w")),
        (b"refs/heads/a/b", on("a/b")),
        (b"refs/heads/nonexistent", on("nonexistent")),
        // git: a ref outside `refs/heads/` — the tool reports only
        // branches
        (b"refs/tags/v1", unknown()),
        (b"refs/heads", unknown()),
        (b"refs/x", unknown()),
        // git: a branch named in bytes that aren't UTF-8
        (b"refs/heads/n\xff", unknown()),
        // git: any other link is read through, as the file it names —
        // an invalid ref name included, relative to the git dir
        (b"refs/heads/x y", on("main")),
        (b"refs/heads/../x", unknown()),
        (b"./refs/heads/w", unknown()),
        (b"loose", at()),
        (b"symref", on("through")),
        (abs_symref.as_os_str().as_bytes(), on("through")),
        (b"nowhere", unknown()),
    ];
    for (link, want) in rows {
        let head = git_dir.join("HEAD");
        let _ = std::fs::remove_file(&head);
        std::os::unix::fs::symlink(OsStr::from_bytes(link), &head).unwrap();
        assert_eq!(read_head(git_dir), want, "{link:?}");
    }
}

#[test]
fn a_refname_is_checked_as_git_checks_it() {
    // each row probed against git 2.47 (`git check-ref-format <name>`)
    let rows: [(&[u8], bool); 34] = [
        (b"refs/heads/x", true),
        (b"refs/x", true),
        (b"refs/tags/v1", true),
        (b"refs/heads/a.b", true),
        (b"refs/heads/x.lockx", true),
        (b"refs/heads/@", true),
        (b"refs/heads/@x", true),
        (b"refs/heads/x@", true),
        (b"refs/heads/x{", true),
        (b"refs/heads/-x", true),
        (b"refs/heads/n\xff", true),
        // one component, or an empty one
        (b"refs", false),
        (b"refs/", false),
        (b"refs/heads/", false),
        (b"refs//x", false),
        (b"refs/heads/x/", false),
        // git's bad bytes
        (b"refs/heads/x y", false),
        (b"refs/heads/x\t", false),
        (b"refs/heads/x\x7f", false),
        (b"refs/heads/x~", false),
        (b"refs/heads/x^", false),
        (b"refs/heads/x:", false),
        (b"refs/heads/x?", false),
        (b"refs/heads/x*", false),
        (b"refs/heads/x[", false),
        (b"refs/heads/x\\y", false),
        // a component starting `.` or ending `.lock`, `..`, `@{`, a
        // trailing `.`, or `@` alone
        (b"refs/heads/.x", false),
        (b"refs/.lock", false),
        (b"refs/heads/x.lock", false),
        (b"refs/heads/x.lock/y", false),
        (b"refs/heads/a..b", false),
        (b"refs/heads/x@{", false),
        (b"refs/heads/x.", false),
        (b"@", false),
    ];
    for (name, valid) in rows {
        assert_eq!(is_valid_refname(name), valid, "{name:?}");
    }
}

#[test]
fn a_worktree_gitdir_is_read_as_git_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path();
    // each row probed against git 2.47 (`git worktree list --porcelain`
    // with the worktree's git dir's `gitdir` holding the bytes): the
    // path it lists, and whether a NUL is left once trimmed
    let rows: [(&[u8], &[u8], bool); 16] = [
        (b"/g/w/.git\n", b"/g/w", false),
        (b"/g/w\n", b"/g/w", false),
        // git's own whitespace trimmed, then `/.git` stripped
        (b"/g/w/.git \t\r\n", b"/g/w", false),
        // not the locale's: a form feed stays, and so does `/.git`
        (b"/g/w/.git\x0c\n", b"/g/w/.git\x0c", false),
        // leading whitespace stays
        (b" /g/w/.git\n", b" /g/w", false),
        // only `/.git` itself, once
        (b"/g/w/.git/\n", b"/g/w/.git/", false),
        (b"/g/w/.git/.git\n", b"/g/w/.git", false),
        (b".git\n", b".git", false),
        // `/.git` stripped from the whole file, then cut at a NUL
        (b"/g/w/.git\0junk\n", b"/g/w/.git", true),
        (b"/g/w\0/.git\n", b"/g/w", true),
        (b"/g/w/.git\n\0\n", b"/g/w/.git\n", true),
        // raw bytes, UTF-8 or not
        (b"/g/w\xff/.git\n", b"/g/w\xff", false),
        // naming nothing
        (b"\n", b"", false),
        (b"", b"", false),
        (b"\0/g/w/.git", b"", true),
        (b" \t\n", b"", false),
    ];
    for (content, path, nul) in rows {
        std::fs::write(git_dir.join("gitdir"), content).unwrap();
        let got = read_gitdir_file(git_dir).unwrap();
        assert_eq!(got.path.as_os_str().as_bytes(), path, "{content:?}");
        assert_eq!(got.nul, nul, "{content:?}");
    }
    // the tool's own limit (git: reads it whole, and lists `/g/w`)
    let mut padded = b"/g/w/.git".to_vec();
    let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
    padded.resize(max + 1, b'\n');
    std::fs::write(git_dir.join("gitdir"), &padded).unwrap();
    let e = read_gitdir_file(git_dir).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    // missing, or not a regular file
    std::fs::remove_file(git_dir.join("gitdir")).unwrap();
    let e = read_gitdir_file(git_dir).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    std::fs::create_dir(git_dir.join("gitdir")).unwrap();
    let e = read_gitdir_file(git_dir).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
}

/// What a `commondir` names, relative to its git dir unless absolute,
/// none, or an error.
#[derive(Debug)]
enum Commondir {
    Names(&'static [u8]),
    Fails,
}

#[test]
fn a_commondir_is_read_as_git_reads_it() {
    use Commondir::{Fails, Names};
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().canonicalize().unwrap();
    let file = git_dir.join("commondir");
    // each row probed against git 2.47 (`git rev-parse --git-common-dir`
    // in a linked worktree whose git dir's `commondir` holds the bytes)
    let rows: [(&[u8], Commondir); 10] = [
        (b"../..\n", Names(b"../..")),
        (b"/c", Names(b"/c")),
        // git: trims trailing line breaks alone, CR included
        (b"../..\r\n", Names(b"../..")),
        // git: keeps a trailing space or tab (then "not a git
        // repository")
        (b"../.. \n", Names(b"../.. ")),
        (b"../..\t\n", Names(b"../..\t")),
        // git: a C string — trimming the end can't reach past a NUL, and
        // nothing before one is the git dir itself
        (b"../..\0junk\n", Names(b"../..")),
        (b"../..\n\0\n", Names(b"../..\n")),
        (b"\0../..", Names(b"")),
        // git: a line break alone is the git dir itself
        (b"\n", Names(b"")),
        // git: "failed to read" an empty one
        (b"", Fails),
    ];
    for (content, want) in rows {
        std::fs::write(&file, content).unwrap();
        let got = read_commondir(&git_dir);
        let ok = match (&got, &want) {
            (Ok(Some(path)), Names(named)) => *path == git_dir.join(OsStr::from_bytes(named)),
            (Err(_), Fails) => true,
            _ => false,
        };
        assert!(ok, "{content:?}: {got:?}, want {want:?}");
    }
    // git reads it whole however large, so one with a NUL in reach is
    // read past any size limit
    let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
    let mut large = b"../c\0".to_vec();
    large.resize(max + C_STRING_CHUNK * 2, b'x');
    std::fs::write(&file, &large).unwrap();
    assert_eq!(
        read_commondir(&git_dir).unwrap(),
        Some(git_dir.join("../c"))
    );
    // the tool's own limit: no NUL in reach (git: follows it)
    let mut padded = b"../c".to_vec();
    padded.resize(max + 1, b'\n');
    std::fs::write(&file, &padded).unwrap();
    assert!(read_commondir(&git_dir).is_err());
    // git: "failed to read" a dir, or a dangling link — it looks with
    // `lstat`, so neither is absent
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert!(read_commondir(&git_dir).is_err());
    std::fs::remove_dir(&file).unwrap();
    std::os::unix::fs::symlink("nowhere", &file).unwrap();
    assert!(read_commondir(&git_dir).is_err());
    // a link to a file is read through
    std::fs::write(git_dir.join("target"), "../..\n").unwrap();
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink("target", &file).unwrap();
    assert_eq!(
        read_commondir(&git_dir).unwrap(),
        Some(git_dir.join("../.."))
    );
    // none: the git dir is its own common dir
    std::fs::remove_file(&file).unwrap();
    assert_eq!(read_commondir(&git_dir).unwrap(), None);
}

#[test]
fn a_head_is_read_as_git_reads_a_loose_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path();
    let on = |name: &str| UnprobedHead::Branch { name: name.into() };
    let id = "0123456789abcdef0123456789abcdef01234567";
    let at = || UnprobedHead::Detached { commit: id.into() };
    let unknown = || UnprobedHead::Unknown;
    // each row probed against git 2.47 (`git worktree list` and a commit
    // in a linked worktree whose git dir's `HEAD` holds the bytes)
    let rows: Vec<(Vec<u8>, UnprobedHead)> = vec![
        (b"ref: refs/heads/main\n".to_vec(), on("main")),
        (b"ref: refs/heads/a/b".to_vec(), on("a/b")),
        // git: any whitespace after `ref:`, none included
        (b"ref:refs/heads/x".to_vec(), on("x")),
        (b"ref:\trefs/heads/x\n".to_vec(), on("x")),
        // git: trims trailing whitespace, CR included
        (b"ref: refs/heads/x \r\n\n".to_vec(), on("x")),
        // git: a C string, cut at the first NUL, and trimming the end
        // can't reach past it (git refuses the name `x `, moving nothing)
        (b"ref: refs/heads/x\0junk\n".to_vec(), on("x")),
        (b"ref: refs/heads/x \0\n".to_vec(), on("x ")),
        // git's own whitespace, not the locale's: a form feed stays (and
        // git refuses the name)
        (b"ref: refs/heads/x\x0c\n".to_vec(), on("x\x0c")),
        // git: an object id, then the end or whitespace and anything
        (format!("{id}\n").into_bytes(), at()),
        (format!("{id} junk\n").into_bytes(), at()),
        (format!("{id}\tjunk").into_bytes(), at()),
        (format!("{id}\0junk").into_bytes(), at()),
        // git: a broken ref, or no git dir at all
        (format!("{id}junk\n").into_bytes(), unknown()),
        (format!(" {id}\n").into_bytes(), unknown()),
        (b" ref: refs/heads/x\n".to_vec(), unknown()),
        (b"\0ref: refs/heads/main\n".to_vec(), unknown()),
        (b"garbage\n".to_vec(), unknown()),
        (b"abc123\n".to_vec(), unknown()),
        (b"ref: main\n".to_vec(), unknown()),
        (b"".to_vec(), unknown()),
        // git's null id: nothing checked out
        (format!("{}\n", "0".repeat(40)).into_bytes(), unknown()),
        // git: a symref outside `refs/heads/` moves no branch, but the
        // tool reports only branches
        (b"ref: refs/tags/v1\n".to_vec(), unknown()),
        (b"ref: refs/remotes/origin/main\n".to_vec(), unknown()),
        // git: a form feed isn't whitespace after `ref:` either
        (b"ref:\x0crefs/heads/x\n".to_vec(), unknown()),
        // git: a branch named in bytes that aren't UTF-8 — reported as
        // unknown, which holds every branch
        (b"ref: refs/heads/\xff\n".to_vec(), unknown()),
    ];
    for (content, want) in rows {
        std::fs::write(git_dir.join("HEAD"), &content).unwrap();
        assert_eq!(read_head(git_dir), want, "{content:?}");
    }
    // git reads it whole however large: a NUL in reach is found
    let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
    let mut large = b"ref: refs/heads/big\0".to_vec();
    large.resize(max + C_STRING_CHUNK * 2, b'x');
    std::fs::write(git_dir.join("HEAD"), &large).unwrap();
    assert_eq!(read_head(git_dir), on("big"));
    // the tool's own limit: no NUL in reach (git: on `main`)
    let mut padded = b"ref: refs/heads/main".to_vec();
    padded.resize(max + 1, b'\n');
    std::fs::write(git_dir.join("HEAD"), &padded).unwrap();
    assert_eq!(read_head(git_dir), unknown());
    // none, or a dir
    std::fs::remove_file(git_dir.join("HEAD")).unwrap();
    assert_eq!(read_head(git_dir), unknown());
    std::fs::create_dir(git_dir.join("HEAD")).unwrap();
    assert_eq!(read_head(git_dir), unknown());
}
