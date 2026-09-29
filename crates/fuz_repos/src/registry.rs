//! The `repos.toml` registry: its core schema, parsed strictly.
//!
//! Core fields belong to this tool, and an unknown one is a parse error with
//! its position. The `grimoire` namespace on a repo belongs to the grimoire
//! and is accepted unread. A reference's `branch` and `pinned` fold into one
//! `CheckoutMode`, so declaring both is a parse error.

use std::collections::BTreeMap;
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
        let invalid = || format!("`{s}` is not an `https://<host>/<account>/<name>` URL");
        let rest = s.strip_prefix("https://").ok_or_else(invalid)?;
        let rest = rest.trim_end_matches('/');
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let mut parts = rest.split('/');
        let (Some(host), Some(account), Some(name), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        if host.is_empty() || account.is_empty() || name.is_empty() {
            return Err(invalid());
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
            message: e.to_string(),
        })
    }

    /// Whether `url`'s account is one of the owners (`is_owner`).
    pub fn is_owned(&self, url: &RepoUrl) -> bool {
        is_owner(&self.owners, &url.account)
    }

    /// Every entry, repos then references, each sorted by key.
    pub fn entries(&self) -> Vec<Entry> {
        let repos = self.repos.iter().map(|(key, r)| Entry {
            key: key.clone(),
            kind: EntryKind::Repo,
            dir: r.dir.clone().unwrap_or_else(|| r.url.name.clone()),
            url: r.url.clone(),
            writable: self.is_owned(&r.url),
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
        let references = self.references.iter().map(|(key, r)| Entry {
            key: key.clone(),
            kind: EntryKind::Reference,
            dir: r.dir.clone().unwrap_or_else(|| r.url.name.clone()),
            url: r.url.clone(),
            writable: self.is_owned(&r.url),
            archived: false,
            visibility: None,
            ci: false,
            checkout_mode: r.checkout.clone(),
        });
        repos.chain(references).collect()
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
        let r = Registry::parse(MINIMAL).unwrap();
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
        .unwrap();
        let entries = r.entries();
        assert!(entries[0].writable);
        assert!(!entries[1].writable);
        assert!(is_owner(&r.owners, "me"));
        assert!(!is_owner(&r.owners, "mee"));
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
        for bad in [
            "git@github.com:a/b",
            "http://github.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com//b",
        ] {
            assert!(RepoUrl::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }
}
