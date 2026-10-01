//! The unregistered scan: the workspace root's children holding a `.git`
//! that no registry entry claims.
//!
//! Direct children only, never recursing. A clone's temp dir
//! (`.<dir>.repos-clone-<pid>-<nonce>`) is reported as the tool's own, `.git`
//! or not: a clone a sync didn't finish, or one still running. A child is
//! skipped when it's a registry dir — by canonical path, so a symlink to a
//! registered dir isn't a stray — or when it's a live checkout of a registered
//! repo git itself knows: a linked worktree whose `.git` file names a git dir
//! under that repo's `<common>/worktrees/` whose `gitdir` names this path back,
//! or the main checkout of a repo whose registry dir is one of its linked
//! worktrees. Anything else of a registered repo is reported, never skipped:
//! moved by hand (`git worktree repair` reconnects it, offered only when the
//! repair — which also walks every other worktree git dir of the repo — would
//! rewrite no other checkout, and no git dir of the repo names its worktree
//! relatively, which git versions resolve differently, or has a `gitdir` the
//! tool can't read), orphaned (its git dir is gone or holds no `HEAD`), or
//! sharing a git dir another checkout uses or may use — a copy, a locked
//! worktree's absent original, a second copy of a moved worktree — where a
//! repair would take the git dir from that checkout, so none is offered.
//!
//! A `.git`, a git dir's `commondir`, and a worktree git dir's `gitdir` are
//! read as git reads them (`gitdir`) — raw bytes, UTF-8 or not — so a child
//! is linked to a registered repo only through files git itself would
//! follow, and a repair's walk is judged over every worktree git walks. A
//! repair is offered only as a command naming the dir exactly: never for a
//! path that isn't UTF-8, nor through a `gitdir` git reads two ways (a NUL
//! in it).
//!
//! Reading is file reads plus one git call per stray, for its origin: `git
//! config` through the hardened runner, so includes, `includeIf`, and a
//! linked worktree's common-dir config apply as git applies them. The scan
//! writes nothing.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use crate::classify::remote_account;
use crate::clone::is_temp_dir_name;
use crate::git::{CallOptions, Git};
use crate::gitdir::{
    GitdirTarget, dot_git_target, read_commondir, read_gitdir_target, read_worktree_git_dirs,
};
use crate::paths::canonical;
use crate::porcelain::after_last_reset;
use crate::registry::{Entry, is_owner};
use crate::report::{RepairBlock, UnregisteredClone, UnregisteredKind};
use crate::url::without_userinfo;

/// What the unregistered scan found.
#[derive(Debug)]
pub struct Scan {
    /// The strays, sorted by name.
    pub unregistered: Vec<UnregisteredClone>,
    /// For each stray, the git dir its `.git` names, canonicalized, when
    /// found and not gone — how `mark_moved_worktrees` links a moved
    /// worktree to the gone one git still lists.
    pub git_dirs: Vec<Option<PathBuf>>,
}

/// Scans `root`'s children for unregistered dirs holding a `.git`, sorted by
/// name. `entries` must be the whole registry — the scan claims a dir for
/// any entry — and `owners` its owner accounts.
///
/// # Errors
///
/// When `root` can't be listed.
pub fn scan_unregistered(
    root: &Path,
    entries: &[Entry],
    owners: &[String],
    git: &Git,
) -> io::Result<Scan> {
    let registered: Vec<Registered> = entries
        .iter()
        .map(|e| Registered::resolve(e, root))
        .collect();
    let mut children = Vec::new();
    for child in std::fs::read_dir(root)? {
        children.push(child?.file_name());
    }
    children.sort();
    let mut strays = Vec::new();
    for name in children {
        let path = root.join(&name);
        // follows symlinks: a link to a dir is scanned as the dir; a plain
        // file, a dangling link, or one that can't be looked at isn't a clone
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let dot_git = path.join(".git");
        // the link itself, not its target: a dangling `.git` is still a
        // `.git`, and one that can't be looked at (an unreadable dir) may be
        let absent = matches!(
            std::fs::symlink_metadata(&dot_git),
            Err(e) if e.kind() == io::ErrorKind::NotFound
        );
        // a clone's temp dir is the tool's leftover, `.git` or not
        let unfinished = name.to_str().is_some_and(is_temp_dir_name);
        if absent && !unfinished {
            continue;
        }
        let real = canonical(&path);
        if registered.iter().any(|r| r.claims(real.as_deref())) {
            continue;
        }
        let stray = if unfinished {
            Stray::at(UnregisteredKind::UnfinishedClone)
        } else {
            let Some(stray) = stray_kind(real.as_deref(), &dot_git, &registered) else {
                continue;
            };
            stray
        };
        strays.push(Found {
            name,
            path,
            real,
            stray,
        });
    }
    let shared = shared_git_dirs(&strays);
    let settled = Settled::new(&strays, &shared, &registered);
    let mut found = Vec::with_capacity(strays.len());
    for (i, f) in strays.iter().enumerate() {
        let origin = read_origin(git, f.stray.config_dir.as_deref().unwrap_or(&f.path));
        // read as git connects for it, then redacted for show
        let owned = origin
            .as_deref()
            .and_then(remote_account)
            .is_some_and(|account| is_owner(owners, &account));
        found.push(UnregisteredClone {
            dir: f.name.to_string_lossy().into_owned(),
            origin: origin.map(|o| without_userinfo(&o).into_owned()),
            owned,
            kind: settled.kind(i),
        });
    }
    Ok(Scan {
        unregistered: found,
        git_dirs: strays.into_iter().map(|f| f.stray.git_dir).collect(),
    })
}

/// The strays' kinds, now that the scan knows every stray: one whose git
/// dir another stray names too shares it (a repair of either would take it
/// from the other); a moved worktree whose `.git` isn't a regular file is
/// one git won't repair cleanly; and a repair of one is offered only when
/// nothing else it would rewrite stands in the way.
struct Settled<'s, 'r> {
    strays: &'s [Found],
    shared: &'s HashMap<usize, usize>,
    registered: &'r [Registered<'r>],
    /// For each stray a repair could reconnect (moved, its `.git` a regular
    /// file, sharing its git dir with no fellow stray), the first checkout
    /// that repair would also rewrite; `None` for the rest.
    hazards: Vec<Option<&'r RepairHazard>>,
}

impl<'s, 'r> Settled<'s, 'r> {
    fn new(
        strays: &'s [Found],
        shared: &'s HashMap<usize, usize>,
        registered: &'r [Registered<'r>],
    ) -> Self {
        let hazards = (0..strays.len())
            .map(|i| {
                repairable(strays, shared, i)
                    .and_then(|(entry, git_dir)| registered[entry].walk(git_dir).hazard)
            })
            .collect();
        Self {
            strays,
            shared,
            registered,
            hazards,
        }
    }

    fn kind(&self, i: usize) -> UnregisteredKind {
        let key = |entry: usize| self.registered[entry].key.to_owned();
        let sharer = self.shared.get(&i).map(|&j| &self.strays[j].path);
        match (&self.strays[i].stray.kind, sharer) {
            (Kind::Settled(kind), _) => kind.clone(),
            (Kind::RegisteredMain { entry } | Kind::Moved { entry, .. }, Some(with)) => {
                UnregisteredKind::SharedGitDir {
                    entry: key(*entry),
                    with: Some(with.to_string_lossy().into_owned()),
                }
            }
            (Kind::RegisteredMain { .. }, None) => UnregisteredKind::Clone,
            // git refuses a repair through a `.git` that isn't a file, and
            // one through a link to a file writes the link's target into the
            // git dir
            (
                Kind::Moved {
                    regular_file: false,
                    ..
                },
                None,
            ) => UnregisteredKind::Worktree,
            (
                Kind::Moved {
                    entry,
                    git_dir,
                    nul,
                    ..
                },
                None,
            ) => {
                let effects = self.registered[*entry].walk(git_dir);
                let shown = |git_dir: &Path| git_dir.to_string_lossy().into_owned();
                // a relative or unreadable `gitdir` anywhere in the repo
                // makes the walk uncertain, whatever else stands in the way;
                // a path the report can't show exactly can't be named in any
                // fix; and a NUL in its own `gitdir` may leave the repair
                // changing nothing
                let blocked_by = effects
                    .relative
                    .map(|git_dir| RepairBlock::RelativeGitdir {
                        git_dir: shown(git_dir),
                    })
                    .or_else(|| {
                        effects
                            .unreadable
                            .map(|git_dir| RepairBlock::UnreadableGitdir {
                                git_dir: shown(git_dir),
                            })
                    })
                    .or_else(|| self.hazards[i].map(|h| self.repair_block(i, h)))
                    .or_else(|| {
                        let exact = self.strays[i].path.to_str().is_some();
                        (!exact).then_some(RepairBlock::NonUtf8Path)
                    })
                    .or_else(|| {
                        nul.then(|| RepairBlock::NulInGitdir {
                            git_dir: shown(git_dir),
                        })
                    });
                let exit_noise = if blocked_by.is_none() {
                    effects.noise.map(|h| shown(&h.shown))
                } else {
                    None
                };
                UnregisteredKind::MovedWorktree {
                    entry: key(*entry),
                    blocked_by,
                    exit_noise,
                }
            }
        }
    }

    /// What stands in the way of stray `i`'s repair: another checkout, or —
    /// when the hazard's dir is the stray's own — the git dir claiming it.
    /// When a stray whose `.git` names that git dir has its own dir claimed
    /// by this one's git dir, the two were swapped (named by the partner's
    /// own name, not an alias's).
    fn repair_block(&self, i: usize, hazard: &RepairHazard) -> RepairBlock {
        let git_dir = hazard.git_dir.to_string_lossy().into_owned();
        let found = &self.strays[i];
        if found.real.as_deref() != Some(hazard.path.as_path()) {
            return RepairBlock::Rewrites {
                path: hazard.shown.to_string_lossy().into_owned(),
                git_dir,
            };
        }
        let partners: Vec<usize> = (0..self.strays.len())
            .filter(|&j| {
                let other = &self.strays[j];
                other.stray.git_dir.as_deref() == Some(hazard.git_dir.as_path())
                    && self.hazards[j].is_some_and(|ph| {
                        other.real.as_deref() == Some(ph.path.as_path())
                            && found.stray.git_dir.as_deref() == Some(ph.git_dir.as_path())
                    })
            })
            .collect();
        let partner = partners
            .iter()
            .copied()
            .find(|&j| !is_link(&self.strays[j].path))
            .or_else(|| partners.first().copied());
        match partner {
            Some(p) => RepairBlock::Swapped {
                git_dir,
                with: self.strays[p].name.to_string_lossy().into_owned(),
            },
            None => RepairBlock::ClaimedDir { git_dir },
        }
    }
}

/// The entry and git dir of a stray a repair could reconnect: moved, its
/// `.git` a regular file, no NUL in its git dir's `gitdir`, and no fellow
/// stray at another path naming its git dir.
fn repairable<'a>(
    strays: &'a [Found],
    shared: &HashMap<usize, usize>,
    i: usize,
) -> Option<(usize, &'a Path)> {
    match &strays[i].stray.kind {
        Kind::Moved {
            entry,
            git_dir,
            regular_file: true,
            nul: false,
        } if !shared.contains_key(&i) => Some((*entry, git_dir.as_path())),
        _ => None,
    }
}

/// Whether a path is a symlink itself — an alias of the checkout it names.
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink())
}

/// A child found to be a stray, by name, joined path, and canonical path.
#[derive(Debug)]
struct Found {
    name: OsString,
    path: PathBuf,
    real: Option<PathBuf>,
    stray: Stray,
}

/// For each stray whose git dir another stray names too, the first other
/// such stray, by index. A symlink to a stray is the same checkout, not
/// another: strays at one canonical path never share.
fn shared_git_dirs(strays: &[Found]) -> HashMap<usize, usize> {
    let mut by_git_dir: HashMap<&Path, Vec<usize>> = HashMap::new();
    for (i, found) in strays.iter().enumerate() {
        if let Some(git_dir) = &found.stray.git_dir {
            by_git_dir.entry(git_dir).or_default().push(i);
        }
    }
    let mut shared = HashMap::new();
    for group in by_git_dir.values() {
        for &i in group {
            if let Some(&other) = group.iter().find(|&&j| strays[j].real != strays[i].real) {
                shared.insert(i, other);
            }
        }
    }
    shared
}

/// A registry entry as the scan matches it.
#[derive(Debug)]
struct Registered<'a> {
    key: &'a str,
    /// `<root>/<dir>`, canonicalized, when it exists.
    canonical_dir: Option<PathBuf>,
    /// The repo's common git dir, canonicalized, read from its `.git` (a dir,
    /// or a file naming a git dir and maybe its `commondir`); `None` when it
    /// can't be resolved — then no worktree matches it.
    common_dir: Option<PathBuf>,
    /// Whether the registry dir is a linked worktree: its own git dir isn't
    /// the common one, so the repo's main checkout is elsewhere.
    linked: bool,
    /// What `git worktree repair` does besides the worktree it's given
    /// (`repair_walk`); read once, on the first moved worktree.
    walk: OnceCell<RepairWalk>,
}

/// A path `git worktree repair` reaches whichever worktree it's given:
/// after that one, git walks every worktree git dir, and for each whose
/// `gitdir` names an existing `path`, it rewrites `path/.git` (a hazard) or
/// complains and leaves it be (noise) — `repair_walk`.
#[derive(Debug)]
struct RepairHazard {
    /// The worktree git dir naming it, canonicalized.
    git_dir: PathBuf,
    /// The path, resolved — what the scan compares.
    path: PathBuf,
    /// The path as the git dir's `gitdir` writes it — what git's messages
    /// name, and what the report shows (never a relative one: a repo with
    /// one gets `RepairBlock::RelativeGitdir` instead).
    shown: PathBuf,
}

impl<'a> Registered<'a> {
    fn resolve(entry: &'a Entry, root: &Path) -> Self {
        let dir = root.join(&entry.dir);
        let git_dir = dot_git_target(&dir.join(".git"))
            .ok()
            .and_then(|g| canonical(&g));
        let common_dir = git_dir.as_deref().and_then(common_dir_of);
        Self {
            key: &entry.key,
            canonical_dir: canonical(&dir),
            linked: git_dir.is_some() && common_dir.is_some() && git_dir != common_dir,
            common_dir,
            walk: OnceCell::new(),
        }
    }

    /// What a repair of the worktree whose git dir is `git_dir` would do to
    /// the repo's other worktrees: the first checkout it would also
    /// rewrite, and the first path git would complain about (exiting 1)
    /// while leaving it be. Its own git dir's is repointed at it first, so
    /// what that named is left alone.
    fn walk(&self, git_dir: &Path) -> RepairEffects<'_> {
        let walk = self.walk.get_or_init(|| {
            self.common_dir
                .as_deref()
                .map_or_else(RepairWalk::default, repair_walk)
        });
        RepairEffects {
            hazard: walk.hazards.iter().find(|h| h.git_dir != git_dir),
            noise: walk.noise.iter().find(|h| h.git_dir != git_dir),
            relative: walk.relative.as_deref(),
            unreadable: walk.unreadable.as_deref(),
        }
    }

    /// Whether a child, canonically `real`, is this entry's dir — itself, or
    /// a symlink to it. A child that exists always canonicalizes, and so
    /// does a registry dir that does, so the canonical form alone decides.
    fn claims(&self, real: Option<&Path>) -> bool {
        real.is_some() && self.canonical_dir.as_deref() == real
    }
}

/// The common git dir of a git dir, canonicalized: the dir its `commondir`
/// names, read as git reads it (`read_commondir`), when it has one — a
/// linked worktree's — else itself. `None` where git stops with an error.
fn common_dir_of(git_dir: &Path) -> Option<PathBuf> {
    match read_commondir(git_dir) {
        Ok(Some(common)) => canonical(&common),
        Ok(None) => canonical(git_dir),
        Err(_) => None,
    }
}

/// What `git worktree repair` does to a repo's worktrees whichever one
/// it's given, read once per repo.
#[derive(Debug, Default)]
struct RepairWalk {
    /// The checkouts it would rewrite.
    hazards: Vec<RepairHazard>,
    /// The paths it would complain about, exiting 1, and leave be.
    noise: Vec<RepairHazard>,
    /// The first worktree git dir whose `gitdir` is relative: git versions
    /// resolve it differently, so the walk itself is uncertain.
    relative: Option<PathBuf>,
    /// The first worktree git dir whose `gitdir` is there but can't be read
    /// (or which can't itself be resolved): what git's walk does with it is
    /// unknown, so the walk is uncertain too.
    unreadable: Option<PathBuf>,
}

/// A repair's effects on the other worktrees, for one worktree's repair.
#[derive(Debug)]
struct RepairEffects<'a> {
    hazard: Option<&'a RepairHazard>,
    noise: Option<&'a RepairHazard>,
    relative: Option<&'a Path>,
    unreadable: Option<&'a Path>,
}

/// The repo-wide part of `git worktree repair`, as git decides it: for each
/// worktree git dir whose `gitdir` can be read and names an existing path,
/// git rewrites the path's `.git` when that's missing, or is a file (links
/// followed, as git follows them here) that doesn't name that git dir — a
/// hazard; and complains, exiting 1 but leaving it be, when the path isn't
/// a dir or its `.git` isn't a file — noise. Every git dir git walks is
/// considered, its `gitdir` read as git reads it (bytes, UTF-8 or not): one
/// that's missing or names nothing is skipped, as git skips it; one that's
/// there but can't be read is kept, and makes the whole walk uncertain.
/// Paths resolve as git 2.48+ resolves them; older gits resolve a relative
/// `gitdir` against the cwd, so the first relative one is kept, and makes
/// the walk uncertain too. `worktrees/` that can't be listed yields
/// nothing — the scan then can't tell, and git couldn't walk it either.
fn repair_walk(common: &Path) -> RepairWalk {
    let mut walk = RepairWalk::default();
    let Ok(worktree_git_dirs) = read_worktree_git_dirs(common) else {
        return walk;
    };
    for w in worktree_git_dirs {
        if w.gitdir.is_relative() && walk.relative.is_none() {
            walk.relative = Some(canonical(&w.dir).unwrap_or_else(|| w.dir.clone()));
        }
        let (path, shown, git_dir) = match (w.gitdir, canonical(&w.dir)) {
            (
                GitdirTarget::Names {
                    worktree, written, ..
                },
                Some(git_dir),
            ) => (worktree, written, git_dir),
            // missing, or naming nothing: git skips it
            (GitdirTarget::Missing(_) | GitdirTarget::Empty, _) => continue,
            (GitdirTarget::Names { .. } | GitdirTarget::Unreadable(_), git_dir) => {
                if walk.unreadable.is_none() {
                    walk.unreadable = Some(git_dir.unwrap_or(w.dir));
                }
                continue;
            }
        };
        // lstat, as git's `file_exists`: a dangling link is there, and noise
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let dot_git = path.join(".git");
        let noise = !path.is_dir() || std::fs::metadata(&dot_git).is_ok_and(|m| !m.is_file());
        let rewrites = !noise
            && dot_git_target(&dot_git)
                .ok()
                .and_then(|t| canonical(&t))
                .is_none_or(|t| t != git_dir);
        let hazard = RepairHazard {
            git_dir,
            path,
            shown,
        };
        if noise {
            walk.noise.push(hazard);
        } else if rewrites {
            walk.hazards.push(hazard);
        }
    }
    walk
}

/// A child found to be a stray, the git dir its `.git` names, and where to
/// read its origin when not in the child itself.
#[derive(Debug)]
struct Stray {
    kind: Kind,
    /// The git dir its `.git` names, canonicalized, when it's one a fellow
    /// stray naming it too would share.
    git_dir: Option<PathBuf>,
    /// A common git dir to read the origin from, when git can't read config
    /// through the child's own `.git` (its git dir is gone, or has no
    /// `HEAD`).
    config_dir: Option<PathBuf>,
}

/// A stray's kind as far as the child alone decides it.
#[derive(Debug)]
enum Kind {
    Settled(UnregisteredKind),
    /// Names a registered repo's main git dir that no checkout is found
    /// using: a clone of its own, unless a fellow stray names it too.
    RegisteredMain {
        entry: usize,
    },
    /// A worktree of a registered repo whose worktree git dir, `git_dir`,
    /// names another path or none: moved by hand.
    Moved {
        entry: usize,
        git_dir: PathBuf,
        /// Whether its `.git` is a regular file, not a link — the only kind a
        /// repair reconnects cleanly (git writes a link's target into the git
        /// dir, and refuses a dir).
        regular_file: bool,
        /// Whether the git dir's `gitdir` holds a NUL, so a repair of this
        /// dir may see it as already naming this dir and change nothing
        /// (`GitdirFile::nul`).
        nul: bool,
    },
}

impl Stray {
    const fn at(kind: UnregisteredKind) -> Self {
        Self {
            kind: Kind::Settled(kind),
            git_dir: None,
            config_dir: None,
        }
    }
}

/// What a child holding `dot_git` (canonically at `real`) is; `None` for a
/// live checkout of a registered repo, which isn't a stray.
fn stray_kind(real: Option<&Path>, dot_git: &Path, registered: &[Registered<'_>]) -> Option<Stray> {
    let entry_of = |common: &Path| {
        registered
            .iter()
            .position(|r| r.common_dir.as_deref() == Some(common))
    };
    // the git dir it names: a `.git` dir (or a link to one) is its own, a
    // `.git` file names one; one git can't read (a dangling link, an
    // unreadable file or dir) is still a `.git` here: never silenced
    let regular_file = std::fs::symlink_metadata(dot_git).is_ok_and(|m| m.is_file());
    let git_dir = if dot_git.is_dir() {
        dot_git.to_owned()
    } else {
        let Ok(git_dir) = dot_git_target(dot_git) else {
            return Some(Stray::at(UnregisteredKind::Worktree));
        };
        git_dir
    };
    if !git_dir.try_exists().unwrap_or(true) {
        // a linked worktree's git dir is `<common>/worktrees/<id>`; a copied
        // submodule's `<super git dir>/modules/<name>` isn't one
        let common = git_dir
            .parent()
            .filter(|p| p.file_name() == Some(OsStr::new("worktrees")))
            .and_then(Path::parent)
            .and_then(canonical);
        let kind = common
            .as_deref()
            .and_then(entry_of)
            .map_or(UnregisteredKind::Worktree, |i| {
                UnregisteredKind::OrphanedWorktree {
                    entry: registered[i].key.to_owned(),
                }
            });
        return Some(Stray {
            kind: Kind::Settled(kind),
            git_dir: None,
            config_dir: common,
        });
    }
    let Some(git_dir) = canonical(&git_dir) else {
        return Some(Stray::at(UnregisteredKind::Worktree));
    };
    let Some(common) = common_dir_of(&git_dir) else {
        return Some(Stray::at(UnregisteredKind::Worktree));
    };
    let linked = git_dir != common;
    let Some(entry) = entry_of(&common) else {
        return Some(Stray {
            git_dir: Some(git_dir),
            ..Stray::at(if linked {
                UnregisteredKind::Worktree
            } else {
                UnregisteredKind::Clone
            })
        });
    };
    let kind = if linked {
        linked_git_dir_kind(
            real,
            git_dir.clone(),
            &common,
            entry,
            registered,
            regular_file,
        )?
    } else {
        main_git_dir_kind(real, &git_dir, entry, registered)?
    };
    // git can't read config through a git dir with no `HEAD`
    let orphaned = matches!(
        kind,
        Kind::Settled(UnregisteredKind::OrphanedWorktree { .. })
    );
    Some(Stray {
        kind,
        git_dir: (!orphaned).then_some(git_dir),
        config_dir: orphaned.then_some(common),
    })
}

/// A child naming `git_dir`, registered entry `entry`'s main git dir; `None`
/// for the main checkout of a repo whose registry dir is a linked worktree
/// of it — the probe reports it as the entry's.
fn main_git_dir_kind(
    real: Option<&Path>,
    git_dir: &Path,
    entry: usize,
    registered: &[Registered<'_>],
) -> Option<Kind> {
    let r = &registered[entry];
    if r.linked && real.is_some() && real == git_dir.parent() {
        return None;
    }
    // shared with a checkout that uses it: the entry's dir, or the main
    // checkout git keeps it in
    let user = [r.canonical_dir.as_deref(), git_dir.parent()]
        .into_iter()
        .flatten()
        .find(|p| uses_git_dir(p, git_dir));
    Some(user.map_or(Kind::RegisteredMain { entry }, |with| {
        Kind::Settled(UnregisteredKind::SharedGitDir {
            entry: r.key.to_owned(),
            with: Some(with.to_string_lossy().into_owned()),
        })
    }))
}

/// A child naming `git_dir`, a linked worktree git dir of registered entry
/// `entry` (whose common git dir is `common`); `None` for a live worktree.
fn linked_git_dir_kind(
    real: Option<&Path>,
    git_dir: PathBuf,
    common: &Path,
    entry: usize,
    registered: &[Registered<'_>],
    regular_file: bool,
) -> Option<Kind> {
    let key = || registered[entry].key.to_owned();
    // only a git dir under `<common>/worktrees/` is one git lists
    if git_dir.parent() != canonical(&common.join("worktrees")).as_deref() {
        return Some(Kind::Settled(UnregisteredKind::Worktree));
    }
    // a lock says the worktree may be on media that isn't mounted: this may
    // be a copy of it, so a repair could take its git dir — never moved
    let locked = git_dir.join("locked").try_exists().unwrap_or(true);
    // repair rewrites `gitdir` alone: without a `HEAD` it can't reconnect
    let has_head = git_dir.join("HEAD").try_exists().unwrap_or(false);
    let unnamed = || {
        Kind::Settled(UnregisteredKind::SharedGitDir {
            entry: key(),
            with: None,
        })
    };
    let gitdir = read_gitdir_target(&git_dir);
    let nul = matches!(gitdir, GitdirTarget::Names { nul: true, .. });
    Some(match gitdir {
        GitdirTarget::Names {
            worktree: named, ..
        } if real == Some(named.as_path()) => {
            return None;
        }
        GitdirTarget::Names {
            worktree: named, ..
        } if locked || uses_git_dir(&named, &git_dir) => {
            Kind::Settled(UnregisteredKind::SharedGitDir {
                entry: key(),
                with: Some(named.to_string_lossy().into_owned()),
            })
        }
        // a `gitdir` that can't be read may name a worktree in use; a locked
        // one's may name one that's absent
        GitdirTarget::Unreadable(_) => unnamed(),
        GitdirTarget::Missing(_) | GitdirTarget::Empty if locked => unnamed(),
        _ if !has_head => Kind::Settled(UnregisteredKind::OrphanedWorktree { entry: key() }),
        // a lost `gitdir`, or one naming a path that doesn't use it: repair
        // rewrites it
        _ => Kind::Moved {
            entry,
            git_dir,
            regular_file,
            nul,
        },
    })
}

/// Whether the checkout at `path` uses `git_dir` — its `.git` resolves there.
/// A `.git` that exists but can't be read counts as yes (failing closed: the
/// caller then never advises a repair that could take it).
fn uses_git_dir(path: &Path, git_dir: &Path) -> bool {
    let dot_git = path.join(".git");
    match dot_git.try_exists() {
        // the path isn't a dir: nothing's checked out there
        Ok(false) => false,
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => false,
        Ok(true) => dot_git_target(&dot_git).map_or(true, |target| {
            canonical(&target).is_some_and(|t| Some(t) == canonical(git_dir))
        }),
        Err(_) => true,
    }
}

/// `remote.origin.url` as git reads it in `dir` — the first value after the
/// last empty one (which resets the list: `after_last_reset`, the rule
/// `ConfigFacts::origin_url` reads), the one a fetch uses, as git
/// holds it — a credential in its userinfo included; `None` when unset, reset,
/// or git fails. Discovery stops at `dir`'s parent, so a `.git` git can't
/// use never resolves to an enclosing repo.
fn read_origin(git: &Git, dir: &Path) -> Option<String> {
    let parent = canonical(dir).and_then(|d| d.parent().map(Path::to_owned));
    let opts = CallOptions {
        ceiling: parent.as_deref().or_else(|| dir.parent()),
        network: None,
        ..CallOptions::default()
    };
    let out = git
        .output(
            dir,
            &["config", "-z", "--get-all", "remote.origin.url"],
            opts,
        )
        .ok()?;
    let out = std::str::from_utf8(&out).ok()?;
    // a valueless `url` prints empty too: it ends the list as porcelain's
    // `OriginUrl::resets` has it
    let values: Vec<&str> = out.strip_suffix('\0').unwrap_or(out).split('\0').collect();
    let origin = after_last_reset(&values, |v| v.is_empty()).first()?;
    Some((*origin).to_owned())
}
