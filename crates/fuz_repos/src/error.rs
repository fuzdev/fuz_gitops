//! The crate's error type, with the exit-code policy beside its variants.
//!
//! Per-entry failures (a git call that fails in one repo) are report data,
//! never an `Error`: these are the failures that stop a whole run.

use std::path::PathBuf;

use thiserror::Error;

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
    /// `git` isn't on `PATH`.
    #[error("git not found on PATH")]
    GitNotFound,
    /// A target that names no registry entry.
    #[error("no registry entry matches `{name}`")]
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
            | Self::GitNotFound
            | Self::UnknownEntry { .. } => 2,
            Self::Io { .. } => 1,
        }
    }

    /// A fix suggestion for the user, when there is one.
    pub const fn hint(&self) -> Option<&'static str> {
        match self {
            Self::MissingCommand => Some("see `repos --help`"),
            Self::RootNotFound { .. } => Some("`--root` names the dir the entries live under"),
            Self::RegistryNotFound { .. } => {
                Some("run inside the workspace, or pass `--registry <path>`")
            }
            Self::GitNotFound => Some("install git 2.44 or newer"),
            // TODO: suggest close keys (pass 2)
            Self::UnknownEntry { .. } => {
                Some("a target is a registry key, an entry's dir name, or a path inside a checkout")
            }
            Self::RegistryRead { .. } | Self::RegistryParse { .. } | Self::Io { .. } => None,
        }
    }
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
            Error::GitNotFound,
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
}
