//! The `repos.toml` registry: its core schema, parsed strictly, then
//! validated.
//!
//! Core fields belong to this tool, and an unknown one is a parse error with
//! its position. The `grimoire` table on a repo is another tool's namespace,
//! accepted unread. A reference's `branch` (where its checkout
//! lives) and `pinned` (who moves HEAD) are independent. The integrity rules
//! the schema can't express — ownership, unique dirs and keys, checkout-list
//! targets — are `Registry::validate`'s, and only its `ValidRegistry` yields
//! entries.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::probe::canonical;

/// The default branch of a repo whose entry doesn't name one.
const DEFAULT_BRANCH: &str = "main";

/// The whole `repos.toml` document.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Accounts whose repos are writable: write authority is derived from an
    /// entry's `url`, never declared.
    pub owners: Vec<String>,
    /// Owned repos, by key. Sorted, so output is deterministic.
    #[serde(default)]
    pub repos: BTreeMap<String, RepoEntry>,
    /// Reference checkouts (owned forks and third-party clones), by key.
    #[serde(default)]
    pub references: BTreeMap<String, ReferenceEntry>,
}

/// An owned repo — a `[repos.<key>]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    pub url: RepoUrl,
    pub dir: Option<String>,
    /// Parsed so the schema is whole under `deny_unknown_fields`; unread, like
    /// `purpose`.
    pub upstream: Option<RepoUrl>,
    /// The default branch; absent means `main`. Never empty (`parse_branch`).
    #[serde(default, deserialize_with = "parse_branch")]
    pub branch: Option<String>,
    pub visibility: Visibility,
    /// Whether the repo runs CI; absent means it does iff it's public.
    pub ci: Option<bool>,
    #[serde(default)]
    pub archived: bool,
    pub purpose: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub consults: Vec<String>,
    /// Another tool's namespace, accepted and ignored.
    #[serde(default, rename = "grimoire")]
    _grimoire: Option<IgnoredAny>,
}

/// A reference checkout — a `[references.<key>]` table.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceEntry {
    pub url: RepoUrl,
    pub dir: Option<String>,
    pub upstream: Option<RepoUrl>,
    pub purpose: String,
    /// A clone recipe (`--depth 1`); an existing full clone is left as is.
    #[serde(default)]
    pub shallow: bool,
    /// The only subtree checked out (cone mode): a relative path of plain
    /// components, checked at parse (`parse_sparse`).
    #[serde(default, deserialize_with = "parse_sparse")]
    pub sparse: Option<String>,
    /// Where the checkout lives: the branch a clone takes, whose history
    /// holds the commits the checkout sits on. Absent leaves HEAD alone;
    /// never empty (`parse_branch`).
    #[serde(default, deserialize_with = "parse_branch")]
    pub branch: Option<String>,
    /// Who moves HEAD: its consumer, never the tool — independent of
    /// `branch`, detached or on a branch.
    #[serde(default)]
    pub pinned: bool,
}

/// A repo's declared visibility on its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
}

/// An HTTPS repo identity, `https://<host>/<account>/<name>`.
///
/// Strict, because every part is spliced into URLs and commands: the host is
/// a plain DNS name — no credentials (`user:token@`), port, or IP-literal
/// brackets, so the SSH form `git@<host>:…` stays well formed and nothing
/// secret rides along into reports or network calls — and the account and
/// name are path segments of letters, digits, `.`, `_`, and `-` (not `.` or
/// `..`, not starting with `-`), so no query, fragment, escape, or
/// whitespace can follow. A trailing `/` and a `.git` suffix are dropped.
///
/// Sealed (`non_exhaustive`): outside this crate a value is made only
/// through that parse (`TryFrom<String>`, or a registry's); its fields
/// stay public, so an edited one is the caller's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
#[non_exhaustive]
pub struct RepoUrl {
    pub host: String,
    pub account: String,
    pub name: String,
}

impl TryFrom<String> for RepoUrl {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Self, String> {
        // never echo credentials back
        let shown = crate::url::without_userinfo(&s);
        let invalid =
            |why: &str| format!("`{shown}` is not an `https://<host>/<account>/<name>` URL: {why}");
        let rest = s
            .strip_prefix("https://")
            .ok_or_else(|| invalid("it must start with https://"))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.contains('@') {
            return Err(invalid(
                "it carries credentials; a registry URL names a repo, never a secret",
            ));
        }
        let rest = rest.trim_end_matches('/');
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let mut parts = rest.split('/');
        let (Some(host), Some(account), Some(name), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid(
                "it must have exactly a host, an account, and a name",
            ));
        };
        if !crate::url::is_plain_host(host) {
            return Err(invalid(
                "the host must be a plain DNS name (no port, credentials, or brackets)",
            ));
        }
        let is_segment = |p: &str| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && !p.starts_with('-')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        if !is_segment(account) || !is_segment(name) {
            return Err(invalid(
                "the account and name must be letters, digits, `.`, `_`, and `-`",
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            account: account.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl RepoUrl {
    /// The SSH form, `git@<host>:<account>/<name>` — how owned repos clone
    /// and push.
    pub(crate) fn ssh(&self) -> String {
        format!("git@{}:{}/{}", self.host, self.account, self.name)
    }

    /// Whether `other` names the same repo, ignoring ASCII case: host
    /// names and GitHub's account and repo paths are case-insensitive.
    fn same_repo(&self, other: &Self) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
            && self.account.eq_ignore_ascii_case(&other.account)
            && self.name.eq_ignore_ascii_case(&other.name)
    }
}

/// A reference's `sparse`, checked as it's parsed: a relative path of plain
/// components, each a directory name `git sparse-checkout set --cone` takes
/// literally — no empty component (so no leading or trailing `/`, and not
/// `""`), no `.` or `..`, no glob character (`*`, `?`, `[`), backslash, or
/// control character. The error names the value, with its position.
fn parse_sparse<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let path = String::deserialize(deserializer)?;
    let plain = |c: &str| {
        !matches!(c, "" | "." | "..")
            && !c.contains(['*', '?', '[', '\\'])
            && !c.chars().any(char::is_control)
    };
    if path.split('/').all(plain) {
        Ok(Some(path))
    } else {
        Err(serde::de::Error::custom(format!(
            "sparse `{}` isn't a relative path of plain directory names (no empty, `.`, or \
             `..` component, no leading or trailing `/`, no `*`, `?`, `[`, `\\`, or control \
             character)",
            path.escape_debug()
        )))
    }
}

/// An entry's `branch`, checked as it's parsed: not empty. An empty one
/// names no branch — not the default, which is the field left out — and
/// would reach git as an empty ref name (`--branch ""`, `refs/heads/`).
/// The error has its position.
fn parse_branch<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let branch = String::deserialize(deserializer)?;
    if branch.is_empty() {
        Err(serde::de::Error::custom(
            "branch is empty; name a branch, or leave `branch` out",
        ))
    } else {
        Ok(Some(branch))
    }
}

impl From<RepoUrl> for String {
    fn from(url: RepoUrl) -> Self {
        url.to_string()
    }
}

impl fmt::Display for RepoUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "https://{}/{}/{}", self.host, self.account, self.name)
    }
}

/// Whether `account` is one of `owners`, ignoring ASCII case: host
/// accounts (GitHub's) are case-insensitive. The one ownership comparison,
/// for registry entries and unregistered clones alike.
pub(crate) fn is_owner(owners: &[String], account: &str) -> bool {
    owners.iter().any(|o| o.eq_ignore_ascii_case(account))
}

/// Which registry table an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Repo,
    Reference,
}

/// One registry entry, repo or reference, with its defaults and derived
/// fields resolved — what the probe and the report work from.
///
/// Sealed (`non_exhaustive`): outside this crate an entry is made only by
/// `ValidRegistry::entries`, so as made its `dir` passed validation (a
/// plain name, safe to join to the root) and `writable` was derived from
/// the owners. Its fields stay public: a caller that edits one owns what
/// it wrote.
// Independent declared facts, not a hidden state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Entry {
    pub key: String,
    pub kind: EntryKind,
    /// The on-disk dir name under the workspace root.
    pub dir: String,
    pub url: RepoUrl,
    /// Whether the `url`'s account is one of the registry's owners.
    pub writable: bool,
    pub archived: bool,
    /// Declared on repos; references declare none.
    pub visibility: Option<Visibility>,
    pub ci: bool,
    /// The branch the checkout lives on: a repo's default branch, kept in
    /// sync with origin; a reference's declared one, else `None`, which
    /// leaves HEAD wherever it is.
    pub branch: Option<String>,
    /// A pinned reference's consumer moves its HEAD, never the tool: a
    /// permanent hold, whether HEAD is detached or on a branch. It's never
    /// fetched, and every fast-forward and move in it is `HeldBy::Pinned`.
    pub pinned: bool,
    /// A reference cloned `--depth 1` when missing; an existing full clone
    /// is left as is. Repos are never shallow.
    pub shallow: bool,
    /// The one subtree a reference's clone checks out (cone mode, cloned
    /// `--filter=blob:none`); `None` checks out the whole tree.
    pub sparse: Option<String>,
    /// The first other entry, in registry order, whose `url` names the same
    /// repo (`RepoUrl::same_repo`): one entry's dir may be a linked worktree
    /// of the other's repo, so a missing one is never cloned
    /// (`NeedsHuman::CloneSharesRepo`). `None` when no other entry does.
    pub same_repo_as: Option<String>,
}

impl Entry {
    /// The URL `origin` should hold: SSH for owned entries, HTTPS for
    /// third-party ones — transport follows write authority.
    pub(crate) fn remote_url(&self) -> String {
        if self.writable {
            self.url.ssh()
        } else {
            self.url.to_string()
        }
    }
}

/// Every registry entry's dir under the workspace root, canonicalized.
///
/// Only those that exist. Two entries can share a repo, one a linked
/// worktree of the other, and a worktree at another entry's dir is never
/// advised away with a branch.
#[derive(Debug, Clone, Default)]
pub struct RegistryDirs(HashSet<PathBuf>);

impl RegistryDirs {
    /// `entries` must be the whole registry.
    pub fn new(root: &Path, entries: &[Entry]) -> Self {
        Self(
            entries
                .iter()
                .filter_map(|e| canonical(&root.join(&e.dir)))
                .collect(),
        )
    }

    /// Whether `path`, canonicalized, is a registry entry's dir; `false` when
    /// it can't be canonicalized.
    pub(crate) fn contains(&self, path: &Path) -> bool {
        canonical(path).is_some_and(|p| self.0.contains(&p))
    }
}

impl Registry {
    /// Parses a registry document strictly.
    ///
    /// # Errors
    ///
    /// Returns the TOML or schema error, with the offending key's position.
    pub(crate) fn parse(src: &str) -> std::result::Result<Self, toml::de::Error> {
        toml::from_str(src)
    }

    /// Reads and parses the registry at `path`.
    ///
    /// # Errors
    ///
    /// `RegistryRead` when the file can't be read, `RegistryParse` when it
    /// doesn't match the core schema.
    pub fn load(path: &Path) -> Result<Self> {
        let src = std::fs::read_to_string(path).map_err(|source| Error::RegistryRead {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&src).map_err(|e| Error::RegistryParse {
            path: path.to_owned(),
            // the parser quotes the offending line, credentials and all
            message: crate::url::redact_userinfo_in(&e.to_string()),
        })
    }

    /// Whether `url`'s account is one of the owners (`is_owner`).
    fn is_owned(&self, url: &RepoUrl) -> bool {
        is_owner(&self.owners, &url.account)
    }

    /// Checks the integrity rules the schema can't express, returning the
    /// registry as a `ValidRegistry` when every one holds, else every issue
    /// found, in a fixed order: unowned repos; per reference, an unowned fork
    /// and a key in both tables; dirs that aren't a plain name; dirs claimed
    /// twice; keys that are another entry's dir; per repo, its `requires` and
    /// `consults` targets.
    ///
    /// # Errors
    ///
    /// Every `RegistryIssue` found, at once.
    pub fn validate(self) -> std::result::Result<ValidRegistry, Vec<RegistryIssue>> {
        let mut issues = Vec::new();
        for (key, repo) in &self.repos {
            if !self.is_owned(&repo.url) {
                issues.push(RegistryIssue::RepoNotOwned {
                    key: key.clone(),
                    account: repo.url.account.clone(),
                });
            }
        }
        for (key, reference) in &self.references {
            if reference.upstream.is_some() && !self.is_owned(&reference.url) {
                issues.push(RegistryIssue::ForkNotOwned { key: key.clone() });
            }
            if self.repos.contains_key(key) {
                issues.push(RegistryIssue::KeyInBoth { key: key.clone() });
            }
        }
        let named = self.named_dirs();
        for (name, dir) in named.iter().filter(|(_, dir)| !is_plain_name(dir)) {
            issues.push(RegistryIssue::DirNotAName {
                entry: name.clone(),
                dir: dir.clone(),
            });
        }
        let mut claimed: BTreeMap<&str, &EntryName> = BTreeMap::new();
        for (name, dir) in &named {
            if let Some(first) = claimed.get(dir.as_str()) {
                issues.push(RegistryIssue::DirClaimedTwice {
                    dir: dir.clone(),
                    first: (*first).clone(),
                    second: name.clone(),
                });
            } else {
                claimed.insert(dir, name);
            }
        }
        // each key once (a key in both tables is `KeyInBoth`'s), and none
        // that an entry under it has as its dir: another entry with that dir
        // is then `DirClaimedTwice`'s — or, under the same key in the other
        // table, `KeyInBoth`'s
        let keys: BTreeSet<&str> = named.iter().map(|(n, _)| n.key.as_str()).collect();
        let own_dir = |key: &str| named.iter().any(|(n, dir)| n.key == key && dir == key);
        for key in keys.into_iter().filter(|k| !own_dir(k)) {
            for (name, _) in named.iter().filter(|(_, dir)| dir == key) {
                issues.push(RegistryIssue::KeyIsOtherDir {
                    key: key.to_owned(),
                    entry: name.clone(),
                });
            }
        }
        for (key, repo) in &self.repos {
            for (field, targets) in [
                (CheckoutList::Requires, &repo.requires),
                (CheckoutList::Consults, &repo.consults),
            ] {
                for target in targets {
                    if target == key {
                        issues.push(RegistryIssue::SelfRef {
                            key: key.clone(),
                            field,
                        });
                    } else if !self.repos.contains_key(target)
                        && !self.references.contains_key(target)
                    {
                        issues.push(RegistryIssue::UnknownCheckoutRef {
                            key: key.clone(),
                            field,
                            target: target.clone(),
                        });
                    }
                }
            }
            for target in repo.consults.iter().filter(|t| repo.requires.contains(t)) {
                issues.push(RegistryIssue::RequiresAndConsults {
                    key: key.clone(),
                    target: target.clone(),
                });
            }
        }
        if issues.is_empty() {
            Ok(ValidRegistry(self))
        } else {
            Err(issues)
        }
    }

    /// Every entry's name and dir, repos then references, each by key.
    fn named_dirs(&self) -> Vec<(EntryName, String)> {
        let repos = self.repos.iter().map(|(key, r)| {
            (
                EntryName::new(EntryKind::Repo, key),
                entry_dir(r.dir.as_ref(), &r.url),
            )
        });
        let references = self.references.iter().map(|(key, r)| {
            (
                EntryName::new(EntryKind::Reference, key),
                entry_dir(r.dir.as_ref(), &r.url),
            )
        });
        repos.chain(references).collect()
    }
}

/// Whether `dir` is one plain name — exactly one normal path component, so
/// joined to the workspace root it names a child of the root: not empty, not
/// `.` or `..`, no `/` or `\` (a separator on some platform), no NUL.
fn is_plain_name(dir: &str) -> bool {
    !matches!(dir, "" | "." | "..") && !dir.contains(['/', '\\', '\0'])
}

/// An entry's dir under the workspace root: `dir`, else the `url`'s name.
fn entry_dir(dir: Option<&String>, url: &RepoUrl) -> String {
    dir.cloned().unwrap_or_else(|| url.name.clone())
}

/// A registry every integrity rule holds for (`Registry::validate`): what
/// the rest of the tool works from, so no code downstream of loading sees
/// an unvalidated one.
#[derive(Debug)]
pub struct ValidRegistry(Registry);

impl ValidRegistry {
    /// Reads, parses, and validates the registry at `path`.
    ///
    /// # Errors
    ///
    /// `RegistryRead` and `RegistryParse` as `Registry::load` returns them;
    /// `RegistryInvalid` with every issue `Registry::validate` finds.
    pub fn load(path: &Path) -> Result<Self> {
        Registry::load(path)?
            .validate()
            .map_err(|issues| Error::RegistryInvalid {
                path: path.to_owned(),
                issues,
            })
    }

    /// The owner accounts, whose repos are writable.
    pub fn owners(&self) -> &[String] {
        &self.0.owners
    }

    /// Every entry, repos then references, each sorted by key.
    pub fn entries(&self) -> Vec<Entry> {
        let registry = &self.0;
        let repos = registry.repos.iter().map(|(key, r)| Entry {
            key: key.clone(),
            kind: EntryKind::Repo,
            dir: entry_dir(r.dir.as_ref(), &r.url),
            url: r.url.clone(),
            writable: registry.is_owned(&r.url),
            archived: r.archived,
            visibility: Some(r.visibility),
            ci: r.ci.unwrap_or(r.visibility == Visibility::Public),
            branch: Some(
                r.branch
                    .clone()
                    .unwrap_or_else(|| DEFAULT_BRANCH.to_owned()),
            ),
            pinned: false,
            shallow: false,
            sparse: None,
            same_repo_as: None,
        });
        let references = registry.references.iter().map(|(key, r)| Entry {
            key: key.clone(),
            kind: EntryKind::Reference,
            dir: entry_dir(r.dir.as_ref(), &r.url),
            url: r.url.clone(),
            writable: registry.is_owned(&r.url),
            archived: false,
            visibility: None,
            ci: false,
            branch: r.branch.clone(),
            pinned: r.pinned,
            shallow: r.shallow,
            sparse: r.sparse.clone(),
            same_repo_as: None,
        });
        let mut entries: Vec<Entry> = repos.chain(references).collect();
        let same: Vec<Option<String>> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                entries
                    .iter()
                    .enumerate()
                    .find(|(j, o)| *j != i && o.url.same_repo(&e.url))
                    .map(|(_, o)| o.key.clone())
            })
            .collect();
        for (e, same) in entries.iter_mut().zip(same) {
            e.same_repo_as = same;
        }
        entries
    }
}

/// An entry by table and key — a key alone is ambiguous when both tables
/// hold it (`RegistryIssue::KeyInBoth`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryName {
    pub kind: EntryKind,
    pub key: String,
}

impl EntryName {
    fn new(kind: EntryKind, key: &str) -> Self {
        Self {
            kind,
            key: key.to_owned(),
        }
    }
}

impl fmt::Display for EntryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let table = match self.kind {
            EntryKind::Repo => "repo",
            EntryKind::Reference => "reference",
        };
        write!(f, "{table} `{}`", self.key)
    }
}

/// A repo's list of the sibling checkouts it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutList {
    /// What its gates and tooling need to run.
    Requires,
    /// What it's read against.
    Consults,
}

impl fmt::Display for CheckoutList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Requires => "requires",
            Self::Consults => "consults",
        })
    }
}

/// A registry integrity rule broken — one the schema can't express.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RegistryIssue {
    /// A `[repos]` entry whose `url` account isn't an owner: repos are the
    /// owned ones, and a third-party clone belongs in `[references]`.
    RepoNotOwned { key: String, account: String },
    /// A reference with an `upstream` whose `url` isn't owned: a fork is an
    /// owned repo.
    ForkNotOwned { key: String },
    /// An entry whose dir — its `dir`, else its `url`'s last segment — isn't
    /// one plain name: empty, `.`, `..`, or holding a `/`, `\`, or NUL. Its
    /// checkout must be a child of the workspace root; anything else would
    /// reach outside it, or nowhere.
    DirNotAName { entry: EntryName, dir: String },
    /// Two entries resolve to one dir; `first` is the earlier in registry
    /// order (repos, then references, each by key).
    DirClaimedTwice {
        dir: String,
        first: EntryName,
        second: EntryName,
    },
    /// A key both tables hold.
    KeyInBoth { key: String },
    /// A key that is another entry's dir, so a target naming it would be
    /// ambiguous — a target resolves as a key before a dir. Not reported
    /// for an entry in the other table under the same key (`KeyInBoth`),
    /// nor when the key's own entry has that dir too (`DirClaimedTwice`).
    KeyIsOtherDir { key: String, entry: EntryName },
    /// A `requires` or `consults` target that names no entry.
    UnknownCheckoutRef {
        key: String,
        field: CheckoutList,
        target: String,
    },
    /// A repo that `requires` or `consults` itself.
    SelfRef { key: String, field: CheckoutList },
    /// A target in both of a repo's lists: a checkout is needed or only read,
    /// not both.
    RequiresAndConsults { key: String, target: String },
}

impl fmt::Display for RegistryIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RepoNotOwned { key, account } => write!(
                f,
                "repo `{key}` sits under `{account}`, not an owner — a third-party clone \
                 belongs in [references]"
            ),
            Self::ForkNotOwned { key } => write!(
                f,
                "reference `{key}` sets `upstream` but its url isn't owned — a fork is an \
                 owned repo"
            ),
            Self::DirNotAName { entry, dir } => write!(
                f,
                "{entry} has dir `{dir}`, which isn't a plain name — an entry's dir is one \
                 directory under the workspace root"
            ),
            Self::DirClaimedTwice { dir, first, second } => {
                write!(f, "{second} claims dir `{dir}`, already claimed by {first}")
            }
            Self::KeyInBoth { key } => write!(f, "`{key}` is both a repo and a reference"),
            Self::KeyIsOtherDir { key, entry } => write!(
                f,
                "key `{key}` is the dir of {entry} — a target naming it is ambiguous"
            ),
            Self::UnknownCheckoutRef { key, field, target } => write!(
                f,
                "repo `{key}` {field} `{target}`, which is neither a repo nor a reference"
            ),
            Self::SelfRef { key, field } => write!(f, "repo `{key}` {field} itself"),
            Self::RequiresAndConsults { key, target } => write!(
                f,
                "repo `{key}` both requires and consults `{target}` — pick one"
            ),
        }
    }
}

#[cfg(test)]
mod tests;
