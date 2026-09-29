//! Git's own files, read exactly as git reads them: a gitfile (a `.git`
//! file naming a git dir), a git dir's `commondir` and `HEAD` (a loose ref,
//! or a symlink naming one), and a worktree git dir's `gitdir` (the
//! worktree it names). The probe, the unregistered scan, and busy
//! attribution all read them here, so each follows what git follows and
//! nothing git refuses.
//!
//! Git reads each as raw bytes, trims a few trailing bytes (line breaks, or
//! its own whitespace — the ASCII space, tab, and line breaks, never the
//! locale's), and then takes what's left as a C string, cut at the first
//! NUL. So do these readers: a path is taken as bytes, UTF-8 or not; only a
//! branch name must be UTF-8 to be reported (else the `HEAD` is `Unknown`,
//! which holds every branch).
//!
//! Git caps a gitfile at 1 MiB (`MAX_GITFILE_BYTES`) and reads a
//! `commondir`, a `HEAD`, or a `gitdir` whole, however large. The tool reads
//! the first two only as far as their first NUL, up to its own limit
//! (`MAX_GIT_C_STRING_BYTES`), and a `gitdir` whole up to that same limit:
//! one past it is an error here where git might follow it, which the
//! callers fail closed on.

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use crate::porcelain::is_object_id;
use crate::regular_file::{open_regular, read_bounded_bytes};
use crate::state::UnprobedHead;

/// The largest gitfile git reads (`read_gitfile_gently`); git refuses a
/// larger one.
const MAX_GITFILE_BYTES: u64 = 1024 * 1024;

/// The most of a `commondir` or a `HEAD` the tool reads looking for a NUL,
/// where git takes the file as a C string. Git reads either whole, however
/// large, so this is the tool's own limit, not git's: a larger one with no
/// NUL in reach is an error, where git might follow it.
const MAX_GIT_C_STRING_BYTES: u64 = 1024 * 1024;

/// How much of a file `read_c_string` reads at a time.
const C_STRING_CHUNK: usize = 8 * 1024;

/// Why a gitfile names no git dir: where git stops with an error, in
/// git's words, naming the gitfile.
#[derive(Debug, thiserror::Error)]
pub enum GitfileError {
    /// It can't be read, isn't a regular file, or is over git's limit.
    #[error("reading {}: {source}", dot_git.display())]
    Unreadable {
        dot_git: PathBuf,
        source: std::io::Error,
    },
    /// It doesn't start `gitdir: `.
    #[error("invalid gitfile format: {}", dot_git.display())]
    InvalidFormat { dot_git: PathBuf },
    /// Nothing follows `gitdir: ` once trailing line breaks are trimmed.
    #[error("no path in gitfile: {}", dot_git.display())]
    NoPath { dot_git: PathBuf },
}

/// The git dir a gitfile names, as git's `read_gitfile_gently` reads it: a
/// regular file (the path followed) of at most `MAX_GITFILE_BYTES` starting
/// `gitdir: `, exactly and on its first line; trailing line breaks dropped
/// from the whole file, which must leave a byte past the prefix; the path
/// the rest up to the first NUL, raw bytes, relative to the file's dir
/// unless absolute. Nothing else is trimmed: a trailing space is part of
/// the path, and so is a second line.
///
/// Whether the path is a git dir isn't checked here; git checks it next,
/// and callers match it against the git dirs they know.
///
/// # Errors
///
/// Where git stops with an error instead (`GitfileError`).
pub fn read_gitfile(dot_git: &Path) -> Result<PathBuf, GitfileError> {
    const PREFIX: &[u8] = b"gitdir: ";
    let bytes = read_bounded_bytes(dot_git, MAX_GITFILE_BYTES).map_err(|source| {
        GitfileError::Unreadable {
            dot_git: dot_git.to_owned(),
            source,
        }
    })?;
    if !bytes.starts_with(PREFIX) {
        return Err(GitfileError::InvalidFormat {
            dot_git: dot_git.to_owned(),
        });
    }
    let trimmed = trim_end(&bytes, is_line_break);
    if trimmed.len() <= PREFIX.len() {
        return Err(GitfileError::NoPath {
            dot_git: dot_git.to_owned(),
        });
    }
    let named = c_path(&trimmed[PREFIX.len()..]);
    Ok(dot_git.parent().unwrap_or(dot_git).join(named))
}

/// The git dir a checkout's `.git` names: the dir itself (a link to one
/// followed), or the one a gitfile there names (`read_gitfile`).
///
/// # Errors
///
/// When it's neither a dir nor a gitfile git would follow, with why in
/// git's words.
pub fn dot_git_target(dot_git: &Path) -> Result<PathBuf, String> {
    if dot_git.is_dir() {
        return Ok(dot_git.to_owned());
    }
    read_gitfile(dot_git).map_err(|e| e.to_string())
}

/// The common dir a git dir's `commondir` names, as git's
/// `get_common_dir_noenv` reads it: the file whole (the path followed),
/// trailing line breaks dropped, a C string (`read_c_string`), relative to
/// the git dir unless absolute; joined, not resolved. `Ok(None)` when there
/// is no `commondir` — nothing at that name, as git looks with `lstat`, so a
/// dangling link is one — and the git dir is its own common dir.
///
/// # Errors
///
/// Where git stops with an error: the file can't be read, isn't a regular
/// one, or is empty. Also when it has no NUL within the tool's own limit,
/// and when the name can't be looked up for a reason other than being
/// absent (a git dir the tool can't search, where git would take the git
/// dir as its own common dir but couldn't read its `HEAD` either).
pub fn read_commondir(git_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let file = git_dir.join("commondir");
    match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let bytes = read_c_string(&file, MAX_GIT_C_STRING_BYTES, is_line_break)?;
    Ok(Some(git_dir.join(OsStr::from_bytes(&bytes))))
}

/// A linked worktree's git dir's `gitdir` file, as git reads it
/// (`read_worktree_gitdir`).
#[derive(Debug)]
pub struct WorktreeGitdir {
    /// The worktree path it names, as written: where git lists the
    /// worktree, and the path `git worktree repair` walks to. Empty when it
    /// names none.
    pub path: PathBuf,
    /// Whether a NUL is left once the end is trimmed. Git then reads the
    /// file two ways: its worktree list strips a `/.git` only from the
    /// whole buffer's end before cutting at the NUL, while `git worktree
    /// repair`, checking the worktree it's given, compares what's before
    /// the NUL with that worktree's `.git` — so a `<w>/.git\0junk` lists the
    /// worktree at `<w>/.git` and yet looks right to a repair of `<w>`,
    /// which then changes nothing.
    pub nul: bool,
}

/// A linked worktree's git dir's (`<commondir>/worktrees/<id>`) `gitdir`
/// file, read as git's `get_linked_worktree` reads it — the path git lists
/// the worktree at, and the one `git worktree repair` walks to: the file
/// whole (the path followed), trailing whitespace dropped (git's own,
/// `is_git_space`), then a trailing `/.git` dropped from what's left, and
/// only then a C string, cut at the first NUL — so `<w>/.git\0junk` names
/// `<w>/.git` itself. Leading whitespace stays, and the path is raw bytes,
/// UTF-8 or not. A relative path is returned as written, unresolved (git
/// 2.48+ resolves it against the git dir, older gits against the cwd).
///
/// An empty path is one git names no worktree by: an empty file (git
/// skips the git dir) or one that's empty once trimmed and cut (git's path
/// is then no existing dir).
///
/// # Errors
///
/// When the file can't be read, isn't a regular one, or is larger than the
/// tool's own limit (`MAX_GIT_C_STRING_BYTES`; git reads it whole, however
/// large) — `NotFound` when there's nothing at that name to read.
pub fn read_worktree_gitdir(git_dir: &Path) -> std::io::Result<WorktreeGitdir> {
    let bytes = read_bounded_bytes(&git_dir.join("gitdir"), MAX_GIT_C_STRING_BYTES)?;
    let trimmed = trim_end(&bytes, is_git_space);
    let stripped = trimmed.strip_suffix(b"/.git").unwrap_or(trimmed);
    Ok(WorktreeGitdir {
        path: c_path(stripped).to_owned(),
        nul: trimmed.contains(&0),
    })
}

/// A git dir's `HEAD`, read as git reads a loose ref (`files_read_raw_ref`,
/// `parse_loose_ref_contents`).
///
/// A symlink (git's oldest form, written still under
/// `core.preferSymlinkRefs`) whose link text starts `refs/` and is a valid
/// ref name (`is_valid_refname`) names that ref, the link not followed: a
/// branch when it's `refs/heads/<name>`. Any other link is read through, as
/// the file it points to, as git falls through to reading it.
///
/// A file is a C string with trailing whitespace dropped (`read_c_string`,
/// git's own whitespace); then `ref:` and any whitespace naming a branch,
/// `refs/heads/<name>`; or an object id followed by the end or whitespace
/// (anything after is ignored, as git ignores it).
///
/// `Unknown` for everything else — a ref outside `refs/heads/`, a name that
/// isn't UTF-8, and a file that can't be read or has no NUL within the
/// tool's limit — so it might be on any branch. A name git refuses in a
/// file (`x y`) is reported as written: git won't move any real branch
/// through it.
pub fn read_head(git_dir: &Path) -> UnprobedHead {
    let head = git_dir.join("HEAD");
    let Ok(meta) = std::fs::symlink_metadata(&head) else {
        return UnprobedHead::Unknown;
    };
    if meta.is_symlink() {
        let Ok(link) = std::fs::read_link(&head) else {
            return UnprobedHead::Unknown;
        };
        let link = link.as_os_str().as_bytes();
        if link.starts_with(b"refs/") && is_valid_refname(link) {
            return head_on(link);
        }
    } else if !meta.is_file() {
        return UnprobedHead::Unknown;
    }
    let Ok(bytes) = read_c_string(&head, MAX_GIT_C_STRING_BYTES, is_git_space) else {
        return UnprobedHead::Unknown;
    };
    parse_head(&bytes)
}

/// A `HEAD` naming the ref `refname`: on a branch when it's
/// `refs/heads/<name>` with a UTF-8 name, else `Unknown`.
fn head_on(refname: &[u8]) -> UnprobedHead {
    refname
        .strip_prefix(b"refs/heads/")
        .and_then(|name| std::str::from_utf8(name).ok())
        .map_or(UnprobedHead::Unknown, |name| UnprobedHead::Branch {
            name: name.to_owned(),
        })
}

/// Whether git's `check_refname_format` accepts `refname` with no flags:
/// two or more `/`-separated components, none empty, starting with `.`, or
/// ending with `.lock`; no `..` or `@{`; no ASCII control byte, space, `~`,
/// `^`, `:`, `?`, `*`, `[`, or `\`; not ending with `.`; and not `@` alone.
/// Bytes past ASCII are accepted, UTF-8 or not.
fn is_valid_refname(refname: &[u8]) -> bool {
    if refname == b"@" || refname.ends_with(b".") {
        return false;
    }
    let mut components = 0;
    for component in refname.split(|&b| b == b'/') {
        let bad_byte = component.iter().any(|&b| {
            b < 0x20
                || matches!(
                    b,
                    b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\' | 0x7f
                )
        });
        let bad = component.is_empty()
            || bad_byte
            || component.starts_with(b".")
            || component.ends_with(b".lock")
            || component.windows(2).any(|w| w == b".." || w == b"@{");
        if bad {
            return false;
        }
        components += 1;
    }
    components >= 2
}

/// A `HEAD`'s C string, trimmed, as `read_head` reads it.
fn parse_head(bytes: &[u8]) -> UnprobedHead {
    if let Some(target) = bytes.strip_prefix(b"ref:") {
        let start = target
            .iter()
            .position(|&b| !is_git_space(b))
            .unwrap_or(target.len());
        return head_on(&target[start..]);
    }
    let end = bytes
        .iter()
        .position(|&b| is_git_space(b))
        .unwrap_or(bytes.len());
    match std::str::from_utf8(&bytes[..end]) {
        Ok(id) if is_object_id(id) => UnprobedHead::Detached {
            commit: id.to_owned(),
        },
        _ => UnprobedHead::Unknown,
    }
}

/// A regular file's contents (the path followed) as git takes a C string
/// from a buffer it read whole and trimmed: the bytes before the first NUL,
/// untrimmed, since trimming the end can't reach past a NUL; or, with no
/// NUL, the whole file less its trailing bytes `trimmed` matches. Read in
/// chunks and stopped at the first NUL, so a file of any size with one
/// early is read as git reads it; one with no NUL in its first `max` bytes
/// is an error, as is an empty one (git refuses an empty `commondir`, and
/// an empty `HEAD` names nothing).
fn read_c_string(path: &Path, max: u64, trimmed: impl Fn(u8) -> bool) -> std::io::Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    let mut bytes = Vec::new();
    let mut chunk = vec![0; C_STRING_CHUNK];
    loop {
        let n = match file.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        if let Some(nul) = chunk[..n].iter().position(|&b| b == 0) {
            bytes.extend_from_slice(&chunk[..nul]);
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("no NUL in its first {max} bytes"),
            ));
        }
    }
    if bytes.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty",
        ));
    }
    let end = trim_end(&bytes, trimmed).len();
    bytes.truncate(end);
    Ok(bytes)
}

/// Whether git's `isspace` holds for `b`: git's own ctype, the ASCII space,
/// tab, and line breaks, not the locale's (no form feed or vertical tab).
const fn is_git_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether `b` is a line break, all git trims from a `commondir` or a
/// gitfile.
const fn is_line_break(b: u8) -> bool {
    matches!(b, b'\n' | b'\r')
}

/// `bytes` less its trailing bytes `trimmed` matches.
fn trim_end(bytes: &[u8], trimmed: impl Fn(u8) -> bool) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| !trimmed(b))
        .map_or(0, |i| i + 1);
    &bytes[..end]
}

/// A path as git takes one from a buffer: a C string, so up to the first
/// NUL, and raw bytes, UTF-8 or not.
fn c_path(bytes: &[u8]) -> &Path {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Path::new(OsStr::from_bytes(&bytes[..end]))
}

#[cfg(test)]
mod tests {
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
            let got = read_worktree_gitdir(git_dir).unwrap();
            assert_eq!(got.path.as_os_str().as_bytes(), path, "{content:?}");
            assert_eq!(got.nul, nul, "{content:?}");
        }
        // the tool's own limit (git: reads it whole, and lists `/g/w`)
        let mut padded = b"/g/w/.git".to_vec();
        let max = usize::try_from(MAX_GIT_C_STRING_BYTES).unwrap();
        padded.resize(max + 1, b'\n');
        std::fs::write(git_dir.join("gitdir"), &padded).unwrap();
        let e = read_worktree_gitdir(git_dir).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        // missing, or not a regular file
        std::fs::remove_file(git_dir.join("gitdir")).unwrap();
        let e = read_worktree_gitdir(git_dir).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        std::fs::create_dir(git_dir.join("gitdir")).unwrap();
        let e = read_worktree_gitdir(git_dir).unwrap_err();
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
}
