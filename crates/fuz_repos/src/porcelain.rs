//! Pure parsers for git's machine-readable output: `status --porcelain=v2
//! -z`, `for-each-ref` with NUL-separated fields, `worktree list --porcelain
//! -z`, and `config -z`.

use std::collections::BTreeMap;

use crate::state::{Head, Uncommitted};

/// What `git status --porcelain=v2 --branch --show-stash -z` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFacts {
    pub head: Head,
    pub uncommitted: Uncommitted,
    pub stashes: u32,
}

/// Parses `git status --porcelain=v2 --branch --show-stash -z`.
///
/// # Errors
///
/// Returns a message when the output is malformed or has no branch header.
pub fn parse_status(out: &[u8]) -> Result<StatusFacts, String> {
    let mut oid = None;
    let mut head_name = None;
    let mut stashes = 0;
    let mut u = Uncommitted::default();
    let mut records = out.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let record =
            std::str::from_utf8(record).map_err(|_| "status: non-UTF-8 record".to_owned())?;
        if let Some(header) = record.strip_prefix("# ") {
            if let Some(v) = header.strip_prefix("branch.oid ") {
                oid = Some(v.to_owned());
            } else if let Some(v) = header.strip_prefix("branch.head ") {
                head_name = Some(v.to_owned());
            } else if let Some(v) = header.strip_prefix("stash ") {
                stashes = v
                    .parse()
                    .map_err(|_| format!("status: bad stash count `{v}`"))?;
            }
            continue;
        }
        let mut chars = record.chars();
        match (chars.next(), chars.next()) {
            (Some(kind @ ('1' | '2')), Some(' ')) => {
                let xy = record
                    .get(2..4)
                    .ok_or_else(|| format!("status: short record `{record}`"))?;
                let mut xy = xy.chars();
                if xy.next() != Some('.') {
                    u.staged += 1;
                }
                if xy.next() != Some('.') {
                    u.unstaged += 1;
                }
                // a rename or copy carries its original path as the next record
                if kind == '2' {
                    records.next();
                }
            }
            (Some('u'), Some(' ')) => u.conflicted += 1,
            (Some('?'), Some(' ')) => u.untracked += 1,
            (Some('!'), Some(' ')) => {}
            _ => return Err(format!("status: unknown record `{record}`")),
        }
    }
    let head_name = head_name.ok_or("status: no branch.head header")?;
    let head = if head_name == "(detached)" {
        Head::Detached {
            commit: oid.ok_or("status: detached with no branch.oid header")?,
        }
    } else {
        Head::Branch { name: head_name }
    };
    Ok(StatusFacts {
        head,
        uncommitted: u,
        stashes,
    })
}

/// The `for-each-ref` format `parse_refs` reads: name, resolved upstream,
/// upstream track, worktree path, committer date — NUL-separated fields,
/// newline-separated records.
pub const REFS_FORMAT: &str = "%(refname:lstrip=2)%00%(upstream)%00%(upstream:track)%00%(worktreepath)%00%(committerdate:unix)";

/// A local branch as `for-each-ref` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefFacts {
    pub name: String,
    /// The resolved upstream ref (`refs/remotes/origin/main`), `None` when git
    /// resolves none — no upstream configured, or one outside the refspec.
    pub upstream_ref: Option<String>,
    pub track: Track,
    pub worktree: Option<String>,
    pub committer_time: u64,
}

/// `%(upstream:track)`: how a branch stands against its resolved upstream.
/// `Even` is also what a branch with no upstream reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Even,
    Ahead(u32),
    Behind(u32),
    Diverged { ahead: u32, behind: u32 },
    Gone,
}

/// Parses `%(upstream:track)` (`[ahead 1, behind 2]`, `[gone]`, or empty).
///
/// # Errors
///
/// Returns a message for anything else.
pub fn parse_track(s: &str) -> Result<Track, String> {
    if s.is_empty() {
        return Ok(Track::Even);
    }
    let bad = || format!("unrecognized upstream track `{s}`");
    let inner = s
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(bad)?;
    if inner == "gone" {
        return Ok(Track::Gone);
    }
    let (mut ahead, mut behind) = (0, 0);
    for part in inner.split(", ") {
        let (word, n) = part.split_once(' ').ok_or_else(bad)?;
        let n = n.parse().map_err(|_| bad())?;
        match word {
            "ahead" => ahead = n,
            "behind" => behind = n,
            _ => return Err(bad()),
        }
    }
    Ok(match (ahead, behind) {
        (0, 0) => Track::Even,
        (a, 0) => Track::Ahead(a),
        (0, b) => Track::Behind(b),
        (a, b) => Track::Diverged {
            ahead: a,
            behind: b,
        },
    })
}

/// Parses `for-each-ref --format=<REFS_FORMAT>`.
///
/// # Errors
///
/// Returns a message when a record is malformed.
pub fn parse_refs(out: &[u8]) -> Result<Vec<RefFacts>, String> {
    let out = std::str::from_utf8(out).map_err(|_| "for-each-ref: non-UTF-8 output".to_owned())?;
    out.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            let [name, upstream, track, worktree, date] = f[..] else {
                return Err(format!(
                    "for-each-ref: expected 5 fields in `{}`",
                    line.replace('\0', "|")
                ));
            };
            let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
            Ok(RefFacts {
                name: name.to_owned(),
                upstream_ref: non_empty(upstream),
                track: parse_track(track)?,
                worktree: non_empty(worktree),
                committer_time: if date.is_empty() {
                    0
                } else {
                    date.parse()
                        .map_err(|_| format!("for-each-ref: bad date `{date}`"))?
                },
            })
        })
        .collect()
}

/// One worktree as `git worktree list --porcelain -z` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    /// The worktree's path as git prints it: the one its `gitdir` file names
    /// (or, for the main worktree, git's view of it), not necessarily with
    /// symlinks resolved.
    pub path: String,
    pub head: WorktreeHead,
    /// `Some` when locked, with the reason (empty when none was given).
    pub locked: Option<String>,
    /// `Some` when git would prune it — its dir is gone — with git's reason.
    pub prunable: Option<String>,
}

/// What a worktree record says its HEAD is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeHead {
    /// The main worktree of a bare repo.
    Bare,
    Detached {
        commit: String,
    },
    /// Checked out on a branch, by short name; possibly unborn.
    Branch {
        name: String,
    },
    /// git couldn't read it: a missing or garbled `HEAD` lists as the null
    /// object id, with `detached` or with no head line at all.
    Unknown,
}

/// Whether `s` is a full object id (SHA-1 or SHA-256 hex) other than the
/// null one.
pub fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b != b'0')
}

/// A record being read: its head lines are folded when it ends.
#[derive(Debug)]
struct PendingRecord {
    path: String,
    oid: Option<String>,
    bare: bool,
    detached: bool,
    branch: Option<String>,
    locked: Option<String>,
    prunable: Option<String>,
}

impl PendingRecord {
    fn finish(self) -> WorktreeRecord {
        let head = if self.bare {
            WorktreeHead::Bare
        } else if let Some(name) = self.branch {
            WorktreeHead::Branch { name }
        } else {
            match self.oid {
                Some(commit) if self.detached && is_object_id(&commit) => {
                    WorktreeHead::Detached { commit }
                }
                _ => WorktreeHead::Unknown,
            }
        };
        WorktreeRecord {
            path: self.path,
            head,
            locked: self.locked,
            prunable: self.prunable,
        }
    }
}

/// Parses `git worktree list --porcelain -z`.
///
/// Each attribute is NUL-terminated and each record ended by an empty
/// attribute; the main worktree comes first. Attributes this parser doesn't
/// know are skipped, so a newer git's additions don't break it. A record
/// whose HEAD git couldn't read is `WorktreeHead::Unknown`, never detached.
///
/// # Errors
///
/// Returns a message on non-UTF-8 output, or a record that doesn't start
/// with its `worktree` path.
pub fn parse_worktrees(out: &[u8]) -> Result<Vec<WorktreeRecord>, String> {
    let out = std::str::from_utf8(out).map_err(|_| "worktree list: non-UTF-8 output".to_owned())?;
    let mut records = Vec::new();
    let mut current: Option<PendingRecord> = None;
    // the output ends with the last record's empty attribute and then the
    // final terminator; `split` yields one trailing empty string past it
    let fields = out.strip_suffix('\0').unwrap_or(out).split('\0');
    for field in fields {
        if field.is_empty() {
            records.extend(current.take().map(PendingRecord::finish));
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            if current.is_some() {
                return Err(format!(
                    "worktree list: record for `{value}` starts before the last one ended"
                ));
            }
            current = Some(PendingRecord {
                path: value.to_owned(),
                oid: None,
                bare: false,
                detached: false,
                branch: None,
                locked: None,
                prunable: None,
            });
            continue;
        }
        let Some(record) = current.as_mut() else {
            return Err(format!("worktree list: `{key}` outside a record"));
        };
        match key {
            "HEAD" => record.oid = Some(value.to_owned()),
            "bare" => record.bare = true,
            "detached" => record.detached = true,
            "branch" => {
                record.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                );
            }
            "locked" => record.locked = Some(value.to_owned()),
            "prunable" => record.prunable = Some(value.to_owned()),
            // anything a newer git adds
            _ => {}
        }
    }
    records.extend(current.map(PendingRecord::finish));
    Ok(records)
}

/// The paths of the gitlinks (mode `160000`: submodules, committed nested
/// repos) in `git ls-files --stage -z`: `<mode> <oid> <stage>\t<path>` per
/// NUL-terminated entry.
///
/// # Errors
///
/// Returns a message on non-UTF-8 output or an entry without its tab.
pub fn parse_gitlinks(out: &[u8]) -> Result<Vec<String>, String> {
    let out = std::str::from_utf8(out).map_err(|_| "ls-files: non-UTF-8 output".to_owned())?;
    let mut gitlinks = Vec::new();
    for entry in out.split('\0').filter(|e| !e.is_empty()) {
        let (meta, path) = entry
            .split_once('\t')
            .ok_or_else(|| format!("ls-files: malformed entry `{entry}`"))?;
        if meta.starts_with("160000 ") {
            gitlinks.push(path.to_owned());
        }
    }
    Ok(gitlinks)
}

/// The config pattern `ConfigFacts::parse` reads.
pub const CONFIG_PATTERN: &str = r"^(branch|remote)\.|^core\.(sparsecheckout|sshcommand)$";

/// A branch's configured upstream: `branch.<b>.remote` and `.merge`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchConfig {
    pub remote: Option<String>,
    pub merge: Option<String>,
}

impl BranchConfig {
    /// The upstream as a user would name it: `origin/main`.
    pub fn display(&self) -> Option<String> {
        let merge = self.merge.as_deref()?;
        let merge = merge.strip_prefix("refs/heads/").unwrap_or(merge);
        Some(
            self.remote
                .as_deref()
                .map_or_else(|| merge.to_owned(), |remote| format!("{remote}/{merge}")),
        )
    }

    /// Whether the upstream is a branch on `origin`.
    pub fn is_origin(&self) -> bool {
        self.remote.as_deref() == Some("origin") && self.merge.is_some()
    }
}

/// A config value, and whether the repo's own config file holds it.
///
/// That file, `<commondir>/config`, is the one `git remote add`, `git remote
/// set-url`, and `git config --unset-all` edit. A value from any other scope
/// (system, global, worktree, the command line) or from a file the repo's
/// config includes is out of their reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValue {
    pub value: String,
    pub in_repo_file: bool,
}

#[cfg(test)]
impl ConfigValue {
    /// A value in the repo's own config file.
    pub fn repo(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            in_repo_file: true,
        }
    }

    /// A value from anywhere else.
    pub fn elsewhere(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            in_repo_file: false,
        }
    }
}

/// One `remote.origin.url` value, and whether the repo's own config file
/// holds it (as `ConfigValue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginUrl {
    /// `None` for a valueless `url` (no `=`), which every git remote command
    /// refuses (`missing value for 'remote.origin.url'`, then `bad config
    /// variable`); `Some("")` for an empty one, which resets the list.
    pub value: Option<String>,
    pub in_repo_file: bool,
}

impl OriginUrl {
    /// Whether it ends the list before it: empty, or valueless (no URL is
    /// usable either way).
    pub fn resets(&self) -> bool {
        self.value.as_deref().is_none_or(str::is_empty)
    }
}

#[cfg(test)]
impl OriginUrl {
    /// A URL in the repo's own config file.
    pub fn repo(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            in_repo_file: true,
        }
    }

    /// A URL from anywhere else.
    pub fn elsewhere(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            in_repo_file: false,
        }
    }

    /// A valueless `url` in the repo's own config file.
    pub const fn valueless() -> Self {
        Self {
            value: None,
            in_repo_file: true,
        }
    }
}

/// Another remote's fetch refspec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRefspec {
    pub remote: String,
    pub refspec: String,
}

/// Where a repo's `remote.origin.*` keys are set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum OriginKeys {
    /// Nowhere: git knows no remote `origin`.
    #[default]
    None,
    /// Only beyond the repo's scope (global or system config): git knows
    /// `origin`, but `git remote add origin` accepts and `git remote set-url
    /// origin` refuses (`No such remote`).
    Elsewhere,
    /// In the repo's scope — its config file, a file that includes, or its
    /// worktree config: `git remote add origin` refuses (`remote origin
    /// already exists`) and `git remote set-url origin` accepts.
    InRepo,
}

/// What the probe needs from a repo's config, read from every scope as git
/// reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFacts {
    /// Every `remote.origin.url` value in the order git reads them, empty
    /// ones included (an empty value resets the list: `origin_url_list`).
    pub origin_urls: Vec<OriginUrl>,
    /// Where `remote.origin.*` keys are set, if anywhere.
    pub origin_keys: OriginKeys,
    /// Every `remote.origin.fetch` refspec, in the order git reads them.
    pub origin_fetch: Vec<ConfigValue>,
    /// Every other remote's fetch refspecs, in the order git reads them.
    pub other_fetch: Vec<RemoteRefspec>,
    pub partial_filter: Option<String>,
    pub sparse: bool,
    /// Whether `core.sshCommand` is set anywhere, so fetch leaves SSH alone.
    pub ssh_command: bool,
    pub branches: BTreeMap<String, BranchConfig>,
}

impl ConfigFacts {
    /// Parses `git config -z --show-scope --show-origin --get-regexp
    /// <CONFIG_PATTERN>`: per entry, `scope`, `origin`, then `key\nvalue` (or
    /// a bare `key` for a valueless boolean), each NUL-terminated.
    /// `is_repo_file` says whether a `file:` origin's path, as git prints it
    /// (relative to the dir git ran in, for the repo's own file), is the
    /// repo's own config file.
    ///
    /// An origin is a path from anywhere on the machine (a global config's,
    /// an include's), so it may not be UTF-8: such an origin is never the
    /// repo's own file — the repo's paths are UTF-8 or it isn't probed — and
    /// its entry parses on, its advice falling to by-hand.
    ///
    /// # Errors
    ///
    /// Returns a message on a non-UTF-8 scope, key, or value, or on output
    /// that isn't whole entries.
    pub fn parse(out: &[u8], is_repo_file: impl Fn(&str) -> bool) -> Result<Self, String> {
        if out.is_empty() {
            return Ok(Self::default());
        }
        let fields: Vec<&[u8]> = out
            .strip_suffix(b"\0")
            .unwrap_or(out)
            .split(|b| *b == 0)
            .collect();
        let entries = fields.chunks_exact(3);
        if !entries.remainder().is_empty() {
            return Err("config: expected scope, origin, and key per entry".to_owned());
        }
        let utf8 = |b: &[u8]| {
            std::str::from_utf8(b)
                .map(str::to_owned)
                .map_err(|_| "config: non-UTF-8 output".to_owned())
        };
        let mut facts = Self::default();
        for entry in entries {
            let [scope, origin, entry] = [entry[0], entry[1], entry[2]];
            let (scope, entry) = (utf8(scope)?, utf8(entry)?);
            let (scope, entry) = (scope.as_str(), entry.as_str());
            // `None`: not UTF-8, so not the repo's own file
            let origin = std::str::from_utf8(origin).ok();
            let (key, value) = entry
                .split_once('\n')
                .map_or((entry, None), |(k, v)| (k, Some(v)));
            let in_repo_scope = matches!(scope, "local" | "worktree");
            let in_repo_file = scope == "local"
                && origin
                    .and_then(|o| o.strip_prefix("file:"))
                    .is_some_and(&is_repo_file);
            let config_value = |v: &str| ConfigValue {
                value: v.to_owned(),
                in_repo_file,
            };
            if key == "core.sparsecheckout" {
                facts.sparse = value.is_none_or(git_bool);
            } else if key == "core.sshcommand" {
                facts.ssh_command = true;
            } else if let Some(rest) = key.strip_prefix("branch.") {
                let Some((name, var)) = rest.rsplit_once('.') else {
                    continue;
                };
                let slot = match var {
                    "remote" => &mut facts.branches.entry(name.to_owned()).or_default().remote,
                    "merge" => &mut facts.branches.entry(name.to_owned()).or_default().merge,
                    _ => continue,
                };
                *slot = value.map(str::to_owned);
            } else if let Some((remote, var)) =
                key.strip_prefix("remote.").and_then(|r| r.rsplit_once('.'))
            {
                // a remote's name may hold dots and slashes (`origin.old`,
                // `origin/fork`): only exactly `origin` is origin
                if remote != "origin" {
                    if var == "fetch"
                        && let Some(refspec) = value
                    {
                        facts.other_fetch.push(RemoteRefspec {
                            remote: remote.to_owned(),
                            refspec: refspec.to_owned(),
                        });
                    }
                    continue;
                }
                facts.origin_keys = if in_repo_scope {
                    OriginKeys::InRepo
                } else {
                    facts.origin_keys.max(OriginKeys::Elsewhere)
                };
                match var {
                    "url" => facts.origin_urls.push(OriginUrl {
                        value: value.map(str::to_owned),
                        in_repo_file,
                    }),
                    "partialclonefilter" => facts.partial_filter = value.map(str::to_owned),
                    "fetch" => facts.origin_fetch.extend(value.map(config_value)),
                    _ => {}
                }
            }
        }
        Ok(facts)
    }

    /// The `remote.origin.url` list git uses: the values after the last
    /// empty one, which resets the list (a valueless one ends it too — no
    /// URL is usable either way).
    ///
    /// The reset is git 2.46's; the floor is 2.44, and on 2.44 and 2.45 an
    /// empty value is just an empty URL, so `[A, ""]` still fetches from `A`
    /// there while this reads no URL (and `status --fetch` skips it). Such a
    /// config is broken either way — it names no usable remote on newer
    /// gits — so this doesn't gate on the version.
    pub fn origin_url_list(&self) -> &[OriginUrl] {
        let start = self
            .origin_urls
            .iter()
            .rposition(OriginUrl::resets)
            .map_or(0, |i| i + 1);
        &self.origin_urls[start..]
    }

    /// The URL git fetches `origin` from: the first of `origin_url_list`;
    /// `None` when that's empty.
    pub fn origin_url(&self) -> Option<&str> {
        self.origin_url_list()
            .first()
            .and_then(|v| v.value.as_deref())
    }
}

fn git_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")
}

#[cfg(test)]
mod tests {
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
            "main\0refs/remotes/origin/main\0[ahead 1]\0/home/me/dev/gro\0",
            "1759000000\n",
            "fork\0\0\0\0",
            "1758000000\n",
            "diff-rework\0refs/remotes/origin/diff-rework\0[gone]\0\0",
            "1757000000\n",
        ]
        .concat();
        let refs = parse_refs(out.as_bytes()).unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(
            refs[0],
            RefFacts {
                name: "main".into(),
                upstream_ref: Some("refs/remotes/origin/main".into()),
                track: Track::Ahead(1),
                worktree: Some("/home/me/dev/gro".into()),
                committer_time: 1_759_000_000,
            }
        );
        assert_eq!(refs[1].upstream_ref, None);
        assert_eq!(refs[1].track, Track::Even);
        assert_eq!(refs[2].track, Track::Gone);
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
}
