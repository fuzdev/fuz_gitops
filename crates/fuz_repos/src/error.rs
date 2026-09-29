//! The crate's error type, with the exit-code policy beside its variants.
//!
//! Per-entry failures (a git call that fails in one repo) are report data,
//! never an `Error`: these are the failures that stop a whole run.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use crate::git::GitVersion;
use crate::registry::RegistryIssue;

/// A failure that stops a run before it has a report.
#[derive(Debug, Error)]
pub enum Error {
    /// No subcommand, and no flag that runs without one.
    #[error("a subcommand is required")]
    MissingCommand,
    /// `--root` names no directory.
    #[error("no workspace root at {}", root.display())]
    RootNotFound { root: PathBuf },
    /// No `repos.toml` in the start dir or any ancestor.
    #[error("no repos.toml found in {} or any parent directory", start.display())]
    RegistryNotFound { start: PathBuf },
    /// The registry exists but couldn't be read.
    #[error("failed to read the registry at {}", path.display())]
    RegistryRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The registry isn't valid TOML or doesn't match the core schema.
    #[error("invalid registry at {}:\n{message}", path.display())]
    RegistryParse { path: PathBuf, message: String },
    /// The registry parses but breaks integrity rules; `issues` holds every
    /// one found (never empty).
    #[error("invalid registry at {}:{}", path.display(), issue_lines(issues))]
    RegistryInvalid {
        path: PathBuf,
        issues: Vec<RegistryIssue>,
    },
    /// `git` isn't on `PATH`.
    #[error("git not found on PATH")]
    GitNotFound,
    /// git is older than the runner supports, or reports a version it can't
    /// read. `found` is the version git reports.
    #[error("git reports version `{found}`; repos needs {required} or newer")]
    GitTooOld { found: String, required: GitVersion },
    /// A target that names no registry entry; `suggestions` are close keys,
    /// best first.
    #[error("unknown target `{name}`")]
    UnknownEntry {
        name: String,
        suggestions: Vec<String>,
    },
    /// Local I/O outside any one entry.
    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// The process exit code: `2` when the caller must change something
    /// before re-running, `1` for everything else.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::MissingCommand
            | Self::RootNotFound { .. }
            | Self::RegistryNotFound { .. }
            | Self::RegistryRead { .. }
            | Self::RegistryParse { .. }
            | Self::RegistryInvalid { .. }
            | Self::GitNotFound
            | Self::GitTooOld { .. }
            | Self::UnknownEntry { .. } => 2,
            Self::Io { .. } => 1,
        }
    }

    /// A fix suggestion for the user, when there is one.
    pub fn hint(&self) -> Option<Cow<'static, str>> {
        let hint = match self {
            Self::MissingCommand => "see `repos --help`",
            Self::RootNotFound { .. } => "`--root` names the dir the entries live under",
            Self::RegistryNotFound { .. } => {
                "run inside the workspace or a checkout of one of its repos, or pass \
                 `--registry <path>`"
            }
            Self::GitNotFound => "install git 2.44 or newer",
            Self::GitTooOld { .. } => {
                "repos sets `GIT_NO_LAZY_FETCH` (git 2.44+) so a local call on a partial \
                 clone never touches the network — upgrade git"
            }
            Self::UnknownEntry { suggestions, .. } if !suggestions.is_empty() => {
                return Some(Cow::Owned(format!(
                    "did you mean: {}",
                    suggestions.join(", ")
                )));
            }
            Self::UnknownEntry { .. } => {
                "a target is a registry key, an entry's dir name, or a path inside a checkout"
            }
            Self::RegistryInvalid { .. } => {
                "fix each issue in the registry; nothing is probed until it validates"
            }
            Self::RegistryRead { .. } | Self::RegistryParse { .. } | Self::Io { .. } => {
                return None;
            }
        };
        Some(Cow::Borrowed(hint))
    }

    /// The message with its `#[source]` chain, `: `-joined — what the binary
    /// prints after `error: `.
    pub fn message(&self) -> String {
        let mut message = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(s) = source {
            message = format!("{message}: {s}");
            source = s.source();
        }
        message
    }

    /// The stable, machine-readable kind and its payload, for the `--json`
    /// error document.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::MissingCommand => ErrorKind::MissingCommand,
            Self::RootNotFound { .. } => ErrorKind::RootNotFound,
            Self::RegistryNotFound { .. } => ErrorKind::RegistryNotFound,
            Self::RegistryRead { .. } => ErrorKind::RegistryRead,
            Self::RegistryParse { .. } => ErrorKind::RegistryParse,
            Self::RegistryInvalid { issues, .. } => ErrorKind::RegistryInvalid {
                issues: issues.clone(),
            },
            Self::GitNotFound => ErrorKind::GitNotFound,
            Self::GitTooOld { found, required } => ErrorKind::GitTooOld {
                found: found.clone(),
                required: required.to_string(),
            },
            Self::UnknownEntry { name, suggestions } => ErrorKind::UnknownEntry {
                name: name.clone(),
                suggestions: suggestions.clone(),
            },
            Self::Io { .. } => ErrorKind::Io,
        }
    }
}

/// An `Error`'s kind, for the `--json` error document.
///
/// Serialized as the `kind` tag — a closed set in snake case, one per `Error`
/// variant — plus the payload a consumer can act on; the rest is in the
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorKind {
    MissingCommand,
    RootNotFound,
    RegistryNotFound,
    RegistryRead,
    RegistryParse,
    /// Every integrity issue found, each tagged by its own `kind`.
    RegistryInvalid {
        issues: Vec<RegistryIssue>,
    },
    GitNotFound,
    /// `found` as git reports it; `required` as `X.Y.Z`.
    GitTooOld {
        found: String,
        required: String,
    },
    /// The target as given, and the close keys suggested, best first.
    UnknownEntry {
        name: String,
        suggestions: Vec<String>,
    },
    Io,
}

/// Each issue on its own indented line, for `RegistryInvalid`'s message.
fn issue_lines(issues: &[RegistryIssue]) -> String {
    let mut out = String::new();
    for issue in issues {
        let _ = write!(out, "\n  {issue}");
    }
    out
}

/// Result alias for the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_by_remediation() {
        let caller_fixes = [
            Error::MissingCommand,
            Error::RootNotFound {
                root: PathBuf::from("/x"),
            },
            Error::RegistryNotFound {
                start: PathBuf::from("/x"),
            },
            Error::RegistryParse {
                path: PathBuf::from("/x/repos.toml"),
                message: String::new(),
            },
            Error::RegistryInvalid {
                path: PathBuf::from("/x/repos.toml"),
                issues: vec![RegistryIssue::KeyInBoth { key: "k".into() }],
            },
            Error::GitNotFound,
            Error::GitTooOld {
                found: "2.40.0".into(),
                required: crate::git::MIN_GIT_VERSION,
            },
            Error::UnknownEntry {
                name: "x".into(),
                suggestions: vec![],
            },
        ];
        for e in caller_fixes {
            assert_eq!(e.exit_code(), 2, "{e}");
        }
        let io = Error::Io {
            context: "x".into(),
            source: std::io::Error::other("x"),
        };
        assert_eq!(io.exit_code(), 1);
    }

    #[test]
    fn unknown_entry_hints_its_suggestions() {
        let e = Error::UnknownEntry {
            name: "gr".into(),
            suggestions: vec!["gro".into(), "grimoire".into()],
        };
        assert_eq!(e.to_string(), "unknown target `gr`");
        assert_eq!(e.hint().as_deref(), Some("did you mean: gro, grimoire"));
        let e = Error::UnknownEntry {
            name: "zzzzzz".into(),
            suggestions: vec![],
        };
        assert!(e.hint().is_some_and(|h| h.contains("registry key")));
    }

    #[test]
    fn registry_invalid_lists_every_issue_and_carries_them() {
        let e = Error::RegistryInvalid {
            path: PathBuf::from("/x/repos.toml"),
            issues: vec![
                RegistryIssue::KeyInBoth { key: "k".into() },
                RegistryIssue::ForkNotOwned { key: "f".into() },
            ],
        };
        assert_eq!(
            e.message(),
            "invalid registry at /x/repos.toml:\n  `k` is both a repo and a reference\n  \
             reference `f` sets `upstream` but its url isn't owned — a fork is an owned repo"
        );
        assert!(e.hint().is_some());
        assert_eq!(
            serde_json::to_value(e.kind()).unwrap(),
            serde_json::json!({
                "kind": "registry_invalid",
                "issues": [
                    {"kind": "key_in_both", "key": "k"},
                    {"kind": "fork_not_owned", "key": "f"},
                ],
            })
        );
    }

    #[test]
    fn git_too_old_names_both_versions_and_why() {
        let e = Error::GitTooOld {
            found: "2.40.0".into(),
            required: crate::git::MIN_GIT_VERSION,
        };
        assert_eq!(
            e.to_string(),
            "git reports version `2.40.0`; repos needs 2.44.0 or newer"
        );
        assert!(e.hint().is_some_and(|h| h.contains("GIT_NO_LAZY_FETCH")));
    }

    #[test]
    fn kinds_are_snake_case_with_their_payloads() {
        let json = |e: &Error| serde_json::to_value(e.kind()).unwrap();
        assert_eq!(
            json(&Error::RegistryNotFound {
                start: PathBuf::from("/x")
            }),
            serde_json::json!({"kind": "registry_not_found"})
        );
        assert_eq!(
            json(&Error::UnknownEntry {
                name: "x".into(),
                suggestions: vec!["y".into()],
            }),
            serde_json::json!({"kind": "unknown_entry", "name": "x", "suggestions": ["y"]})
        );
        assert_eq!(
            json(&Error::GitTooOld {
                found: "2.40.0".into(),
                required: crate::git::MIN_GIT_VERSION,
            }),
            serde_json::json!({"kind": "git_too_old", "found": "2.40.0", "required": "2.44.0"})
        );
        let io = Error::Io {
            context: "failed to list".into(),
            source: std::io::Error::other("denied"),
        };
        assert_eq!(json(&io), serde_json::json!({"kind": "io"}));
        assert_eq!(io.message(), "failed to list: denied");
    }
}
