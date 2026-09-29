//! The `repos.toml` registry: its core schema, parsed strictly, then
//! validated.
//!
//! Core fields belong to this tool, and an unknown one is a parse error with
//! its position. The `grimoire` namespace on a repo belongs to the grimoire
//! and is accepted unread. A reference's `branch` and `pinned` fold into one
//! `CheckoutMode`, so declaring both is a parse error. The integrity rules
//! the schema can't express — ownership, unique dirs and keys, checkout-list
//! targets — are `Registry::validate`'s, and only its `ValidRegistry` yields
//! entries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The default branch of a repo whose entry doesn't name one.
pub const DEFAULT_BRANCH: &str = "main";

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
    pub upstream: Option<RepoUrl>,
    /// The default branch; absent means `main`.
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
    /// The grimoire's namespace, accepted and ignored.
    #[serde(default, rename = "grimoire")]
    _grimoire: Option<IgnoredAny>,
}

/// A reference checkout — a `[references.<key>]` table.
#[derive(Debug, Deserialize)]
#[serde(try_from = "RawReference")]
pub struct ReferenceEntry {
    pub url: RepoUrl,
    pub dir: Option<String>,
    pub upstream: Option<RepoUrl>,
    pub purpose: String,
    /// A clone recipe (`--depth 1`); an existing full clone is left as is.
    pub shallow: bool,
    /// The only subtree checked out (cone mode).
    pub sparse: Option<String>,
    pub checkout: CheckoutMode,
}

/// The reference table as written; `deny_unknown_fields` lives here because
/// serde ignores it on a `try_from` container.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReference {
    url: RepoUrl,
    dir: Option<String>,
    upstream: Option<RepoUrl>,
    purpose: String,
    #[serde(default)]
    shallow: bool,
    sparse: Option<String>,
    branch: Option<String>,
    #[serde(default)]
    pinned: bool,
}

impl TryFrom<RawReference> for ReferenceEntry {
    type Error = String;

    fn try_from(raw: RawReference) -> std::result::Result<Self, String> {
        let checkout = match (raw.branch, raw.pinned) {
            (Some(_), true) => {
                return Err(
                    "`pinned` and `branch` are mutually exclusive: a pinned checkout is detached"
                        .into(),
                );
            }
            (Some(branch), false) => CheckoutMode::Follow { branch },
            (None, true) => CheckoutMode::Pinned,
            (None, false) => CheckoutMode::Head,
        };
        Ok(Self {
            url: raw.url,
            dir: raw.dir,
            upstream: raw.upstream,
            purpose: raw.purpose,
            shallow: raw.shallow,
            sparse: raw.sparse,
            checkout,
        })
    }
}

/// What a checkout's HEAD is supposed to do. Repos always follow a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckoutMode {
    /// On this branch, kept in sync with its remote.
    Follow { branch: String },
    /// Detached at a commit its consumer pins: never moved.
    Pinned,
    /// Leave HEAD wherever it is.
    Head,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
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
        let is_host = |h: &str| {
            h.starts_with(|c: char| c.is_ascii_alphanumeric())
                && h.ends_with(|c: char| c.is_ascii_alphanumeric())
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        };
        if !is_host(host) {
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
    pub fn ssh(&self) -> String {
        format!("git@{}:{}/{}", self.host, self.account, self.name)
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
pub fn is_owner(owners: &[String], account: &str) -> bool {
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
#[derive(Debug, Clone)]
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
    pub checkout_mode: CheckoutMode,
}

impl Entry {
    /// The URL `origin` should hold: SSH for owned entries, HTTPS for
    /// third-party ones — transport follows write authority.
    pub fn remote_url(&self) -> String {
        if self.writable {
            self.url.ssh()
        } else {
            self.url.to_string()
        }
    }
}

impl Registry {
    /// Parses a registry document strictly.
    ///
    /// # Errors
    ///
    /// Returns the TOML or schema error, with the offending key's position.
    pub fn parse(src: &str) -> std::result::Result<Self, toml::de::Error> {
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
    pub fn is_owned(&self, url: &RepoUrl) -> bool {
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
            checkout_mode: CheckoutMode::Follow {
                branch: r
                    .branch
                    .clone()
                    .unwrap_or_else(|| DEFAULT_BRANCH.to_owned()),
            },
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
            checkout_mode: r.checkout.clone(),
        });
        repos.chain(references).collect()
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
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
owners = ["me"]

[repos.app]
url = "https://github.com/me/app"
visibility = "public"
purpose = "an app"
grimoire.lore_id = "app"
grimoire.frontend = {framework = "sveltekit", deployed = true}

[repos.site]
url = "https://github.com/me/private_site.git"
dir = "site"
branch = "trunk"
visibility = "private"
ci = true
archived = true
purpose = "a site"
requires = ["spec"]

[references.spec]
url = "https://github.com/me/spec"
upstream = "https://github.com/them/spec"
purpose = "a fork"
shallow = true
sparse = "css"
branch = "fork"

[references.oracle]
url = "https://codeberg.org/them/oracle"
purpose = "pinned"
pinned = true

[references.loose]
url = "https://github.com/them/loose/"
purpose = "leave HEAD"
"#;

    #[test]
    fn parses_core_and_ignores_grimoire() {
        let r = Registry::parse(MINIMAL).unwrap();
        assert_eq!(r.owners, ["me"]);
        assert_eq!(r.repos.len(), 2);
        assert_eq!(r.references.len(), 3);
        let site = &r.repos["site"];
        assert_eq!(site.url.name, "private_site");
        assert_eq!(site.dir.as_deref(), Some("site"));
        assert_eq!(site.requires, ["spec"]);
    }

    #[test]
    fn folds_checkout_mode() {
        let r = Registry::parse(MINIMAL).unwrap();
        assert_eq!(
            r.references["spec"].checkout,
            CheckoutMode::Follow {
                branch: "fork".into()
            }
        );
        assert_eq!(r.references["oracle"].checkout, CheckoutMode::Pinned);
        assert_eq!(r.references["loose"].checkout, CheckoutMode::Head);
    }

    #[test]
    fn resolves_entries() {
        let r = Registry::parse(MINIMAL).unwrap().validate().unwrap();
        let entries = r.entries();
        let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["app", "site", "loose", "oracle", "spec"]);

        let app = &entries[0];
        assert_eq!(app.dir, "app");
        assert!(app.writable && app.ci && !app.archived);
        assert_eq!(
            app.checkout_mode,
            CheckoutMode::Follow {
                branch: "main".into()
            }
        );

        let site = &entries[1];
        assert_eq!(site.dir, "site");
        assert!(site.ci && site.archived);
        assert_eq!(
            site.checkout_mode,
            CheckoutMode::Follow {
                branch: "trunk".into()
            }
        );

        let loose = &entries[2];
        assert_eq!(loose.dir, "loose");
        assert!(!loose.writable && !loose.ci && loose.visibility.is_none());

        let spec = &entries[4];
        assert!(spec.writable);
    }

    #[test]
    fn ownership_ignores_case() {
        let r = Registry::parse(
            r#"
owners = ["Me"]
[repos.x]
url = "https://github.com/ME/x"
visibility = "public"
purpose = "x"
[references.y]
url = "https://github.com/them/y"
purpose = "y"
"#,
        )
        .unwrap()
        .validate()
        .unwrap();
        let entries = r.entries();
        assert!(entries[0].writable);
        assert!(!entries[1].writable);
        assert!(is_owner(r.owners(), "me"));
        assert!(!is_owner(r.owners(), "mee"));
    }

    #[test]
    fn private_repo_defaults_ci_off() {
        let r = Registry::parse(
            r#"
owners = ["me"]
[repos.x]
url = "https://github.com/me/x"
visibility = "private"
purpose = "x"
"#,
        )
        .unwrap()
        .validate()
        .unwrap();
        assert!(!r.entries()[0].ci);
    }

    #[test]
    fn unknown_repo_key_is_an_error_with_position() {
        let e = Registry::parse(
            r#"
owners = ["me"]
[repos.x]
url = "https://github.com/me/x"
visibility = "public"
purpose = "x"
brnach = "dev"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("brnach"), "{e}");
        assert!(e.contains("line 7"), "{e}");
    }

    #[test]
    fn unknown_reference_key_is_an_error_with_position() {
        let e = Registry::parse(
            r#"
owners = ["me"]
[references.x]
url = "https://github.com/them/x"
purpose = "x"
grimoire.lore_id = "x"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("grimoire"), "{e}");
        assert!(e.contains("line 6"), "{e}");
    }

    #[test]
    fn unknown_top_level_key_is_an_error() {
        let e = Registry::parse("owners = []\nowner = \"me\"\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("owner"), "{e}");
    }

    #[test]
    fn pinned_with_branch_is_an_error() {
        let e = Registry::parse(
            r#"
owners = []
[references.x]
url = "https://github.com/them/x"
purpose = "x"
pinned = true
branch = "main"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("mutually exclusive"), "{e}");
    }

    #[test]
    fn bad_visibility_is_an_error() {
        let e = Registry::parse(
            r#"
owners = []
[repos.x]
url = "https://github.com/me/x"
visibility = "internal"
purpose = "x"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("internal"), "{e}");
    }

    #[test]
    fn repo_url_parsing() {
        let ok = |s: &str| RepoUrl::try_from(s.to_owned()).unwrap();
        let u = ok("https://github.com/fuzdev/fuz_util");
        assert_eq!(
            (u.host.as_str(), u.account.as_str(), u.name.as_str()),
            ("github.com", "fuzdev", "fuz_util")
        );
        assert_eq!(ok("https://github.com/a/b.git/").name, "b");
        assert_eq!(
            ok("https://github.com/a/b.git").to_string(),
            "https://github.com/a/b"
        );
        assert_eq!(ok("https://codeberg.org/a/b").ssh(), "git@codeberg.org:a/b");
        assert_eq!(ok("https://github.com/org/.github").name, ".github");
        assert_eq!(
            ok("https://git.example-host.org/a.b/c_d-e").host,
            "git.example-host.org"
        );
        for bad in [
            "git@github.com:a/b",
            "http://github.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com//b",
            // credentials, a port, an IP literal, an odd host
            "https://user:sekrit@github.com/a/b",
            "https://token@github.com/a/b",
            "https://github.com:443/a/b",
            "https://[::1]/a/b",
            "https://-github.com/a/b",
            "https://github.com./a/b",
            "https://git hub.com/a/b",
            // a query, a fragment, an escape, whitespace, dot segments
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#frag",
            "https://github.com/a/b%2F",
            "https://github.com/a/b c",
            "https://github.com/a/b\n",
            "https://github.com/../b",
            "https://github.com/a/.",
            "https://github.com/-a/b",
        ] {
            assert!(RepoUrl::try_from(bad.to_owned()).is_err(), "{bad}");
        }
        // a rejected URL never echoes its credentials
        let e = RepoUrl::try_from("https://user:sekrit@github.com/a/b".to_owned()).unwrap_err();
        assert!(!e.contains("sekrit") && !e.contains("user"), "{e}");
        assert!(e.contains("carries credentials"), "{e}");
    }

    /// The issues `src` validates to (none when valid).
    fn issues(src: &str) -> Vec<RegistryIssue> {
        Registry::parse(src)
            .unwrap()
            .validate()
            .err()
            .unwrap_or_default()
    }

    fn name(kind: EntryKind, key: &str) -> EntryName {
        EntryName::new(kind, key)
    }

    #[test]
    fn a_valid_registry_has_no_issues() {
        assert_eq!(issues(MINIMAL), []);
    }

    #[test]
    fn a_repo_is_owned() {
        let got = issues(
            r#"
owners = ["me"]
[repos.theirs]
url = "https://github.com/them/theirs"
visibility = "public"
purpose = "x"
[repos.mine]
url = "https://github.com/ME/mine"
visibility = "public"
purpose = "x"
"#,
        );
        // ownership ignores case, as write authority does
        assert_eq!(
            got,
            [RegistryIssue::RepoNotOwned {
                key: "theirs".into(),
                account: "them".into()
            }]
        );
        assert_eq!(
            got[0].to_string(),
            "repo `theirs` sits under `them`, not an owner — a third-party clone belongs in \
             [references]"
        );
    }

    #[test]
    fn a_fork_is_owned() {
        let got = issues(
            r#"
owners = ["me"]
[references.theirs]
url = "https://github.com/them/theirs"
upstream = "https://github.com/origin/theirs"
purpose = "x"
[references.mine]
url = "https://github.com/me/mine"
upstream = "https://github.com/them/mine"
purpose = "x"
[references.plain]
url = "https://github.com/them/plain"
purpose = "a third-party clone, no fork"
"#,
        );
        assert_eq!(
            got,
            [RegistryIssue::ForkNotOwned {
                key: "theirs".into()
            }]
        );
    }

    #[test]
    fn a_dir_is_one_plain_name() {
        let bad = |dir: &str| {
            let toml = format!(
                "owners = [\"me\"]\n[references.r]\nurl = \"https://github.com/them/r\"\n\
                 purpose = \"x\"\ndir = {}\n",
                // a JSON string is a valid TOML basic string
                serde_json::to_string(dir).unwrap()
            );
            issues(&toml)
        };
        for dir in [
            "", ".", "..", "../x", "x/..", "a/b", "/abs", "a/", "./a", "a\\b", "..\\x", "a\0b",
        ] {
            assert_eq!(
                bad(dir),
                [RegistryIssue::DirNotAName {
                    entry: name(EntryKind::Reference, "r"),
                    dir: dir.to_owned(),
                }],
                "{dir:?}"
            );
        }
        for dir in [
            "r",
            "private_site",
            "tsv.fuz.dev",
            ".hidden",
            "..x",
            "x..",
            "sp ace",
        ] {
            assert_eq!(bad(dir), [], "{dir:?}");
        }
    }

    #[test]
    fn a_url_with_a_dot_segment_name_is_rejected_at_parse() {
        // the dir a URL's name would give is never `.` or `..`: parse refuses
        // the URL before validation could see such a dir
        for url in ["https://github.com/me/..", "https://github.com/them/."] {
            let src =
                format!("owners = [\"me\"]\n[references.r]\nurl = \"{url}\"\npurpose = \"x\"\n");
            let e = Registry::parse(&src).unwrap_err().to_string();
            assert!(e.contains("the account and name must be"), "{e}");
        }
    }

    #[test]
    fn a_dir_is_claimed_once() {
        let got = issues(
            r#"
owners = ["me"]
[repos.a]
url = "https://github.com/me/shared"
visibility = "public"
purpose = "x"
[repos.b]
url = "https://github.com/me/b"
dir = "shared"
visibility = "public"
purpose = "x"
[references.c]
url = "https://github.com/them/shared.git"
purpose = "x"
"#,
        );
        // each later claimant against the first, in registry order
        // not also `KeyIsOtherDir`: no key is `shared`
        assert_eq!(
            got,
            [
                RegistryIssue::DirClaimedTwice {
                    dir: "shared".into(),
                    first: name(EntryKind::Repo, "a"),
                    second: name(EntryKind::Repo, "b"),
                },
                RegistryIssue::DirClaimedTwice {
                    dir: "shared".into(),
                    first: name(EntryKind::Repo, "a"),
                    second: name(EntryKind::Reference, "c"),
                },
            ]
        );
        assert_eq!(
            got[1].to_string(),
            "reference `c` claims dir `shared`, already claimed by repo `a`"
        );
    }

    #[test]
    fn a_key_is_in_one_table() {
        let got = issues(
            r#"
owners = ["me"]
[repos.x]
url = "https://github.com/me/x"
dir = "x-repo"
visibility = "public"
purpose = "x"
[references.x]
url = "https://github.com/them/x"
dir = "x-ref"
purpose = "x"
"#,
        );
        assert_eq!(got, [RegistryIssue::KeyInBoth { key: "x".into() }]);
    }

    #[test]
    fn a_key_is_no_other_entrys_dir() {
        let got = issues(
            r#"
owners = ["me"]
[repos.site]
url = "https://github.com/me/private_site"
visibility = "public"
purpose = "x"
[repos.old]
url = "https://github.com/me/old"
dir = "site"
visibility = "public"
purpose = "x"
[references.self]
url = "https://github.com/them/elsewhere"
dir = "self"
purpose = "a key naming its own dir is fine"
"#,
        );
        assert_eq!(
            got,
            [RegistryIssue::KeyIsOtherDir {
                key: "site".into(),
                entry: name(EntryKind::Repo, "old"),
            }]
        );
        assert_eq!(
            got[0].to_string(),
            "key `site` is the dir of repo `old` — a target naming it is ambiguous"
        );
    }

    #[test]
    fn a_dir_claimed_twice_under_a_claimants_key_is_said_once() {
        let got = issues(
            r#"
owners = ["me"]
[repos.app]
url = "https://github.com/me/app"
visibility = "public"
purpose = "x"
[repos.copy]
url = "https://github.com/me/copy"
dir = "app"
visibility = "public"
purpose = "x"
"#,
        );
        assert_eq!(
            got,
            [RegistryIssue::DirClaimedTwice {
                dir: "app".into(),
                first: name(EntryKind::Repo, "app"),
                second: name(EntryKind::Repo, "copy"),
            }]
        );
    }

    #[test]
    fn a_key_in_both_tables_is_not_also_the_other_ones_dir() {
        // the reference's dir is the repo's key, but under the same key:
        // `KeyInBoth` says it once
        let got = issues(
            r#"
owners = ["me"]
[repos.x]
url = "https://github.com/me/x-app"
visibility = "public"
purpose = "x"
[references.x]
url = "https://github.com/them/x"
purpose = "x"
"#,
        );
        assert_eq!(got, [RegistryIssue::KeyInBoth { key: "x".into() }]);
    }

    #[test]
    fn checkout_lists_name_other_entries_once() {
        let got = issues(
            r#"
owners = ["me"]
[repos.app]
url = "https://github.com/me/app"
visibility = "public"
purpose = "x"
requires = ["app", "spec", "nowhere"]
consults = ["spec", "app", "gone"]
[references.spec]
url = "https://github.com/them/spec"
purpose = "x"
"#,
        );
        assert_eq!(
            got,
            [
                RegistryIssue::SelfRef {
                    key: "app".into(),
                    field: CheckoutList::Requires,
                },
                RegistryIssue::UnknownCheckoutRef {
                    key: "app".into(),
                    field: CheckoutList::Requires,
                    target: "nowhere".into(),
                },
                RegistryIssue::SelfRef {
                    key: "app".into(),
                    field: CheckoutList::Consults,
                },
                RegistryIssue::UnknownCheckoutRef {
                    key: "app".into(),
                    field: CheckoutList::Consults,
                    target: "gone".into(),
                },
                RegistryIssue::RequiresAndConsults {
                    key: "app".into(),
                    target: "spec".into(),
                },
                // itself in both lists: both rules say so, as the TS gate does
                RegistryIssue::RequiresAndConsults {
                    key: "app".into(),
                    target: "app".into(),
                },
            ]
        );
        let lines: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            [
                "repo `app` requires itself",
                "repo `app` requires `nowhere`, which is neither a repo nor a reference",
                "repo `app` consults itself",
                "repo `app` consults `gone`, which is neither a repo nor a reference",
                "repo `app` both requires and consults `spec` — pick one",
                "repo `app` both requires and consults `app` — pick one",
            ]
        );
    }

    #[test]
    fn every_issue_at_once_in_rule_order() {
        let got = issues(
            r#"
owners = ["me"]
[repos.b]
url = "https://github.com/them/b"
visibility = "public"
purpose = "x"
requires = ["zz"]
[repos.a]
url = "https://github.com/me/a"
dir = "b"
visibility = "public"
purpose = "x"
[references.a]
url = "https://github.com/them/fork"
upstream = "https://github.com/else/fork"
purpose = "x"
[references.z]
url = "https://github.com/them/z"
dir = "a"
purpose = "x"
[references.up]
url = "https://github.com/them/up"
dir = "../up"
purpose = "x"
"#,
        );
        let kinds: Vec<String> = got
            .iter()
            .map(|i| serde_json::to_value(i).unwrap()["kind"].to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "\"repo_not_owned\"",
                "\"fork_not_owned\"",
                "\"key_in_both\"",
                "\"dir_not_a_name\"",
                "\"dir_claimed_twice\"",
                "\"key_is_other_dir\"",
                "\"unknown_checkout_ref\"",
            ]
        );
    }

    #[test]
    fn issues_serialize_kind_tagged() {
        let json = |i: &RegistryIssue| serde_json::to_value(i).unwrap();
        assert_eq!(
            json(&RegistryIssue::DirClaimedTwice {
                dir: "d".into(),
                first: name(EntryKind::Repo, "a"),
                second: name(EntryKind::Reference, "b"),
            }),
            serde_json::json!({
                "kind": "dir_claimed_twice",
                "dir": "d",
                "first": {"kind": "repo", "key": "a"},
                "second": {"kind": "reference", "key": "b"},
            })
        );
        assert_eq!(
            json(&RegistryIssue::UnknownCheckoutRef {
                key: "a".into(),
                field: CheckoutList::Consults,
                target: "z".into(),
            }),
            serde_json::json!({
                "kind": "unknown_checkout_ref",
                "key": "a",
                "field": "consults",
                "target": "z",
            })
        );
    }
}
