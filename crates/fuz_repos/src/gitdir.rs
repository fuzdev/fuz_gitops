//! Git's own files, read exactly as git reads them: a gitfile (a `.git`
//! file naming a git dir), a git dir's `commondir` and `HEAD` (a loose ref,
//! or a symlink naming one), and a worktree git dir's `gitdir` (the
//! worktree it names) — a worktree git dir being a linked worktree's own
//! git dir, `<commondir>/worktrees/<id>`, listed here with what each
//! names (`read_worktree_git_dirs`). The probe, the unregistered scan, and
//! busy attribution all read them here, so each follows what git follows
//! and nothing git refuses.
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

use crate::paths::realpath_forgiving;
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

/// A linked worktree's git dir, `<commondir>/worktrees/<id>` — the
/// worktree's own `$GIT_DIR`, where it keeps its `HEAD`, index, and
/// operation state — and what its `gitdir` file names.
#[derive(Debug)]
pub struct WorktreeGitDir {
    pub dir: PathBuf,
    pub gitdir: GitdirTarget,
}

/// Every worktree git dir under `<commondir>/worktrees/`, readable or not,
/// sorted by path; entries that aren't dirs are skipped, as git skips them
/// (and prune deletes them).
///
/// # Errors
///
/// When `worktrees/` exists but can't be listed.
pub fn read_worktree_git_dirs(common_dir: &Path) -> std::io::Result<Vec<WorktreeGitDir>> {
    let dirs = match std::fs::read_dir(common_dir.join("worktrees")) {
        Ok(dirs) => dirs,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut git_dirs = Vec::new();
    for d in dirs {
        let dir = d?.path();
        // one that can't be stat'd stays, failing closed
        if std::fs::metadata(&dir).is_ok_and(|m| !m.is_dir()) {
            continue;
        }
        let gitdir = read_gitdir_target(&dir);
        git_dirs.push(WorktreeGitDir { dir, gitdir });
    }
    git_dirs.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(git_dirs)
}

/// What a worktree git dir's `gitdir` file names, read as git reads it
/// (`read_gitdir_file`), with the read error's kind kept: a missing or
/// empty file is lost (`git worktree repair` rewrites it), any other
/// failure may hide a worktree in use.
#[derive(Debug)]
pub enum GitdirTarget {
    /// A worktree path. `written` is the path as git takes it — raw bytes,
    /// trailing whitespace (git's own) trimmed, a trailing `/.git`
    /// stripped, then cut at the first NUL (`read_gitdir_file`) — never
    /// empty: what git's messages name. `worktree` is that joined to the
    /// git dir (git 2.48+ resolves a relative one there) and resolved as
    /// far as it exists (`paths::realpath_forgiving`): how git 2.48+'s
    /// worktree list derives the path it prints, so the two compare equal
    /// even when the worktree is gone. `nul` when a NUL is left in it once
    /// trimmed, so a repair of the worktree reads it otherwise
    /// (`GitdirFile::nul`).
    Names {
        worktree: PathBuf,
        written: PathBuf,
        nul: bool,
    },
    /// The file names no path: it's empty, or nothing's left once trimmed
    /// and cut.
    Empty,
    /// No `gitdir` file (the read's error, for messages).
    Missing(std::io::Error),
    /// A `gitdir` file that's there but can't be read, so the tool can't
    /// tell what worktree git names by it.
    Unreadable(std::io::Error),
}

impl GitdirTarget {
    /// Whether it names its worktree by a relative path, which git 2.48+
    /// resolves against the git dir and older gits against the cwd.
    pub fn is_relative(&self) -> bool {
        matches!(self, Self::Names { written, .. } if written.is_relative())
    }
}

/// Reads what a worktree git dir's `gitdir` file names (`GitdirTarget`).
pub fn read_gitdir_target(git_dir: &Path) -> GitdirTarget {
    match read_gitdir_file(git_dir) {
        Ok(file) if file.path.as_os_str().is_empty() => GitdirTarget::Empty,
        Ok(file) => GitdirTarget::Names {
            worktree: realpath_forgiving(&git_dir.join(&file.path)),
            written: file.path,
            nul: file.nul,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GitdirTarget::Missing(e),
        Err(e) => GitdirTarget::Unreadable(e),
    }
}

/// A worktree git dir's `gitdir` file, as git reads it
/// (`read_gitdir_file`).
#[derive(Debug)]
struct GitdirFile {
    /// The worktree path it names, as written: where git lists the
    /// worktree, and the path `git worktree repair` walks to. Empty when it
    /// names none.
    path: PathBuf,
    /// Whether a NUL is left once the end is trimmed. Git then reads the
    /// file two ways: its worktree list strips a `/.git` only from the
    /// whole buffer's end before cutting at the NUL, while `git worktree
    /// repair`, checking the worktree it's given, compares what's before
    /// the NUL with that worktree's `.git` — so a `<w>/.git\0junk` lists the
    /// worktree at `<w>/.git` and yet looks right to a repair of `<w>`,
    /// which then changes nothing.
    nul: bool,
}

/// A worktree git dir's (`<commondir>/worktrees/<id>`) `gitdir` file,
/// read as git's `get_linked_worktree` reads it — the path git lists the
/// worktree at, and the one `git worktree repair` walks to: the file whole
/// (the path followed), trailing whitespace dropped (git's own,
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
fn read_gitdir_file(git_dir: &Path) -> std::io::Result<GitdirFile> {
    let bytes = read_bounded_bytes(&git_dir.join("gitdir"), MAX_GIT_C_STRING_BYTES)?;
    let trimmed = trim_end(&bytes, is_git_space);
    let stripped = trimmed.strip_suffix(b"/.git").unwrap_or(trimmed);
    Ok(GitdirFile {
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
pub fn is_valid_refname(refname: &[u8]) -> bool {
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
mod tests;
