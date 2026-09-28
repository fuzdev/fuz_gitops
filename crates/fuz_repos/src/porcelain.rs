//! Pure parsers for git's machine-readable output: `status --porcelain=v2
//! -z`, `for-each-ref` with NUL-separated fields, and `config -z`.

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

/// What the probe needs from a repo's config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFacts {
    pub origin_url: Option<String>,
    pub partial_filter: Option<String>,
    pub sparse: bool,
    /// Whether `core.sshCommand` is set anywhere, so fetch leaves SSH alone.
    pub ssh_command: bool,
    pub branches: BTreeMap<String, BranchConfig>,
}

impl ConfigFacts {
    /// Parses `git config -z --get-regexp <CONFIG_PATTERN>`: `key\nvalue` per
    /// NUL-terminated entry, or a bare `key` for a valueless boolean.
    ///
    /// # Errors
    ///
    /// Returns a message on non-UTF-8 output.
    pub fn parse(out: &[u8]) -> Result<Self, String> {
        let out = std::str::from_utf8(out).map_err(|_| "config: non-UTF-8 output".to_owned())?;
        let mut facts = Self::default();
        for entry in out.split('\0').filter(|e| !e.is_empty()) {
            let (key, value) = entry
                .split_once('\n')
                .map_or((entry, None), |(k, v)| (k, Some(v)));
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
            } else if let Some(rest) = key.strip_prefix("remote.origin.") {
                match rest {
                    "url" if facts.origin_url.is_none() => {
                        facts.origin_url = value.map(str::to_owned);
                    }
                    "partialclonefilter" => facts.partial_filter = value.map(str::to_owned),
                    _ => {}
                }
            }
        }
        Ok(facts)
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
    fn config_entries() {
        let out = z(&[
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
        let c = ConfigFacts::parse(&out).unwrap();
        assert_eq!(c.origin_url.as_deref(), Some("git@github.com:ryanatkn/wpt"));
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
        let c = ConfigFacts::parse(&z(&["core.sparsecheckout", "core.sshcommand\nssh -i key"]))
            .unwrap();
        assert!(c.sparse && c.ssh_command);
        let c = ConfigFacts::parse(&z(&["core.sparsecheckout\nfalse"])).unwrap();
        assert!(!c.sparse);
    }
}
