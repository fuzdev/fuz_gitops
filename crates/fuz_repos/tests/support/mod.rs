//! `FixtureWorkspace`: a tempdir workspace of real git repos for the
//! integration tests — a bare "remote" per repo, an upstream author's clone
//! that pushes to it, a clone per registry entry, and a generated
//! `repos.toml`.
//!
//! Every git call is hermetic: the environment is cleared down to `PATH`, a
//! throwaway `HOME`, no global or system config, fixed identities, and a
//! fixed clock that advances a minute per call, so commit times are
//! deterministic. Remotes are `file://` URLs, so `--depth` and `--filter`
//! apply and nothing reaches the network. The library runs under the same
//! environment through `Git::with_clean_env`, and the binary through
//! `FixtureWorkspace::command`.
//!
//! A clone's `origin` holds the URL the registry expects (SSH for owned
//! entries, HTTPS for third-party ones) — the probe reads it raw from config
//! — and a repo-local `url.<file URL>.insteadOf` sends fetches to the local
//! bare remote; `GIT_ALLOW_PROTOCOL=file` makes any other transport an error.
//! The visibility check reads under `visibility_base`, a `file://` dir, so it
//! stays local too.
//!
//! Setups assert the git state they build (`assert_track` and kin) before the
//! tool reads it: a setup that silently builds the wrong state tests nothing.
//! `snapshot_git_dir` and `assert_git_dir_unchanged` pin that a tool call
//! wrote nothing to a git dir.

// each test binary uses a subset of the helpers
#![allow(dead_code)]
// test support: a setup that can't build its fixture fails the test, as an
// assertion would
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use fuz_repos::discover::{REGISTRY_FILE, find_registry};
use fuz_repos::git::Git;
use fuz_repos::probe::RegistryDirs;
use fuz_repos::registry::{Entry, ValidRegistry};
use fuz_repos::report::{EntryStatus, UnregisteredClone};
use fuz_repos::scan::scan_unregistered;
use fuz_repos::state::{BranchStatus, UnprobedWorktree};
use fuz_repos::status::{StatusOptions, status};
use tempfile::TempDir;

/// The registry's owner account: its repos are writable.
pub const OWNER: &str = "me";
/// A third-party account: its repos are read-only references.
pub const THIRD_PARTY: &str = "them";
/// The fixture clock's start, in unix seconds.
pub const CLOCK_START: u64 = 1_700_000_000;
/// How far the clock moves per git call.
const TICK: u64 = 60;

/// A workspace of fixture repos under one tempdir.
#[derive(Debug)]
pub struct FixtureWorkspace {
    /// Held for its drop, which deletes the tree.
    _tmp: TempDir,
    /// The tempdir's canonical path: a symlinked `TMPDIR` (macOS) would
    /// otherwise make git's resolved paths differ from the fixture's.
    base: PathBuf,
    clock: Cell<u64>,
    /// The registry's tables, in declaration order.
    tables: Vec<String>,
}

impl Default for FixtureWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureWorkspace {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for dir in ["ws", "remotes", "upstream", "home"] {
            std::fs::create_dir(base.join(dir)).unwrap();
        }
        Self {
            _tmp: tmp,
            base,
            clock: Cell::new(CLOCK_START),
            tables: Vec::new(),
        }
    }

    /// The workspace root, where the registry and the clones live.
    pub fn root(&self) -> PathBuf {
        self.base.as_path().join("ws")
    }

    /// The tempdir holding the workspace, the remotes, and the upstream
    /// clones.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// A dir under the tempdir but outside the workspace.
    pub fn outside(&self, name: &str) -> PathBuf {
        self.base.as_path().join(name)
    }

    /// An entry dir under the workspace root.
    pub fn dir(&self, dir: &str) -> PathBuf {
        self.root().join(dir)
    }

    /// The bare remote for a repo name.
    pub fn bare(&self, name: &str) -> PathBuf {
        self.base
            .as_path()
            .join("remotes")
            .join(format!("{name}.git"))
    }

    /// The upstream author's clone of a repo name.
    pub fn upstream(&self, name: &str) -> PathBuf {
        self.base.as_path().join("upstream").join(name)
    }

    fn file_url(&self, name: &str) -> String {
        format!("file://{}", self.bare(name).display())
    }

    // --- the environment ---

    /// The only environment fixture git calls, the library, and the binary
    /// see (minus the clock, which `command` adds).
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        let home = self.base.as_path().join("home");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), home.clone().into()),
            ("XDG_CONFIG_HOME".into(), home.into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_AUTHOR_NAME".into(), "Fixture Author".into()),
            ("GIT_AUTHOR_EMAIL".into(), "author@example.com".into()),
            ("GIT_COMMITTER_NAME".into(), "Fixture Committer".into()),
            ("GIT_COMMITTER_EMAIL".into(), "committer@example.com".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("LC_ALL".into(), "C".into()),
            // nothing but the local bare remotes, ever
            ("GIT_ALLOW_PROTOCOL".into(), "file".into()),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            env.push(("PATH".into(), path));
        }
        env
    }

    /// The library's runner, under the hermetic environment.
    pub fn runner(&self) -> Git {
        Git::with_clean_env(self.env())
    }

    /// Advances the clock and returns the new time.
    fn tick(&self) -> u64 {
        let t = self.clock.get() + TICK;
        self.clock.set(t);
        t
    }

    /// A command under the hermetic environment, with the clock's next time
    /// as the author and committer date.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
        let date = format!("@{} +0000", self.tick());
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .envs(self.env())
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(cwd)
            .stdin(Stdio::null());
        cmd
    }

    /// Runs git in `cwd` and returns its output whatever the exit status.
    pub fn git_output(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("git", cwd).args(args).output().unwrap()
    }

    /// Runs git in `cwd`, asserting success; returns trimmed stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_raw(cwd, args).trim().to_owned()
    }

    /// Runs git in `cwd`, asserting success; returns stdout as is.
    pub fn git_raw(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git_output(cwd, args);
        assert!(
            out.status.success(),
            "git {} in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs git in `cwd`, asserting it fails (a stopped rebase, a conflict).
    pub fn git_fails(&self, cwd: &Path, args: &[&str]) {
        let out = self.git_output(cwd, args);
        assert!(
            !out.status.success(),
            "git {} in {} unexpectedly succeeded",
            args.join(" "),
            cwd.display()
        );
    }

    // --- remotes and upstream history ---

    /// Creates the bare remote for `name` with one commit on `main` (a
    /// `README` plus `files`), pushed from the upstream author's clone.
    pub fn remote(&self, name: &str, files: &[(&str, &str)]) {
        let bare = self.bare(name);
        let bare_s = bare.to_str().unwrap();
        self.git(
            self.base.as_path(),
            &["-c", "init.defaultBranch=main", "init", "--bare", bare_s],
        );
        // serve `--filter` for partial clones
        self.git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let up = self.upstream(name);
        let up_s = up.to_str().unwrap();
        self.git(
            self.base.as_path(),
            &["-c", "init.defaultBranch=main", "init", up_s],
        );
        self.git(&up, &["remote", "add", "origin", &self.file_url(name)]);
        write(&up, "README", &format!("# {name}\n"));
        for (path, content) in files {
            write(&up, path, content);
        }
        self.git(&up, &["add", "-A"]);
        self.git(&up, &["commit", "-q", "-m", "initial"]);
        self.git(&up, &["push", "-q", "-u", "origin", "main"]);
    }

    /// Commits a change on `branch` upstream and pushes it; the branch is
    /// created from `main` when it doesn't exist. Returns the commit.
    pub fn upstream_commit(&self, name: &str, branch: &str) -> String {
        let up = self.upstream(name);
        if self.has_ref(&up, branch) {
            self.git(&up, &["checkout", "-q", branch]);
        } else {
            self.git(&up, &["checkout", "-q", "-b", branch, "main"]);
        }
        let oid = self.commit(&up, &format!("upstream-{branch}"));
        self.git(&up, &["push", "-q", "origin", branch]);
        self.git(&up, &["checkout", "-q", "main"]);
        oid
    }

    /// Deletes `branch` on the remote.
    pub fn upstream_delete_branch(&self, name: &str, branch: &str) {
        self.git(
            &self.upstream(name),
            &["push", "-q", "origin", "--delete", branch],
        );
    }

    // --- clones ---

    /// Clones `name` into the workspace as an owned entry would be: `origin`
    /// is the registry's SSH URL, fetched through the local bare remote.
    pub fn clone_owned(&self, dir: &str, name: &str, args: &[&str]) -> PathBuf {
        self.clone_as(dir, name, &owned_origin(name), args)
    }

    /// An owned repo in one step: its remote (a `README` plus `files`), its
    /// `[repos.<name>]` entry, and its clone at `<root>/<name>`.
    pub fn owned_repo(&mut self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        self.remote(name, files);
        self.declare_repo(name, name, "");
        self.clone_owned(name, name, &[])
    }

    /// Clones `name` as a third-party reference: `origin` is its HTTPS URL.
    pub fn clone_third_party(&self, dir: &str, name: &str, args: &[&str]) -> PathBuf {
        self.clone_as(dir, name, &third_party_origin(name), args)
    }

    /// Clones `name` into `<root>/<dir>` with `args`, sets `origin` to
    /// `origin`, and routes fetches for it to the bare remote.
    pub fn clone_as(&self, dir: &str, name: &str, origin: &str, args: &[&str]) -> PathBuf {
        let dest = self.dir(dir);
        let url = self.file_url(name);
        let mut clone = vec!["clone", "-q"];
        clone.extend(args);
        clone.extend([url.as_str(), dest.to_str().unwrap()]);
        self.git(&self.root(), &clone);
        self.set_origin(&dest, name, origin);
        dest
    }

    /// Points `origin` at `url` while fetches still reach `name`'s bare
    /// remote.
    pub fn set_origin(&self, repo: &Path, name: &str, url: &str) {
        self.git(repo, &["remote", "set-url", "origin", url]);
        let key = format!("url.{}.insteadOf", self.file_url(name));
        self.git(repo, &["config", &key, url]);
        assert_eq!(self.git(repo, &["config", "remote.origin.url"]), url);
        // what a fetch actually reaches
        assert_eq!(
            self.git(repo, &["ls-remote", "--get-url", "origin"]),
            self.file_url(name)
        );
    }

    /// Adds a linked worktree of `repo` at `path` (`args` follow it: a
    /// branch to check out, `-b <new>`, `--detach`, …) and returns its own
    /// git dir, `<commondir>/worktrees/<id>`.
    pub fn add_worktree(&self, repo: &Path, path: &Path, args: &[&str]) -> PathBuf {
        let mut all = vec!["worktree", "add", "-q", path.to_str().unwrap()];
        all.extend(args);
        self.git(repo, &all);
        assert!(path.join(".git").is_file(), "{} is linked", path.display());
        let admin = PathBuf::from(self.git(path, &["rev-parse", "--absolute-git-dir"]));
        let common = PathBuf::from(self.git(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        assert_eq!(admin.parent(), Some(common.join("worktrees").as_path()));
        admin
    }

    /// The `git worktree list --porcelain` record for the worktree at
    /// `path`, as its attribute lines.
    pub fn worktree_record(&self, repo: &Path, path: &Path) -> Vec<String> {
        let out = self.git_raw(repo, &["worktree", "list", "--porcelain"]);
        let head = format!("worktree {}", path.display());
        out.split("\n\n")
            .map(|r| r.lines().map(str::to_owned).collect::<Vec<_>>())
            .find(|r| r.first() == Some(&head))
            .unwrap_or_else(|| panic!("no worktree {} in:\n{out}", path.display()))
    }

    /// Writes a file and commits it; returns the commit.
    pub fn commit(&self, repo: &Path, label: &str) -> String {
        let n = self.clock.get();
        write(repo, &format!("{label}.txt"), &format!("{label} {n}\n"));
        self.git(repo, &["add", "-A"]);
        self.git(repo, &["commit", "-q", "-m", label]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    /// The committer time of `rev`, in unix seconds.
    pub fn committer_time(&self, repo: &Path, rev: &str) -> u64 {
        self.git(repo, &["log", "-1", "--format=%ct", rev])
            .parse()
            .unwrap()
    }

    // --- the registry ---

    /// Declares an owned public repo at `https://github.com/me/<name>`;
    /// `extra` is more TOML for its table (`dir`, `branch`, `archived`).
    pub fn declare_repo(&mut self, key: &str, name: &str, extra: &str) {
        self.declare_repo_as(key, name, "public", extra);
    }

    /// Declares a repo at `url` as written, with a `visibility`.
    pub fn declare_repo_url(&mut self, key: &str, url: &str, visibility: &str) {
        self.tables.push(format!(
            "[repos.{key}]\nurl = \"{url}\"\nvisibility = \"{visibility}\"\n\
             purpose = \"fixture\"\n"
        ));
    }

    /// Declares an owned repo with a `visibility` (`public`, `private`).
    pub fn declare_repo_as(&mut self, key: &str, name: &str, visibility: &str, extra: &str) {
        self.tables.push(format!(
            "[repos.{key}]\nurl = \"https://github.com/{OWNER}/{name}\"\n\
             visibility = \"{visibility}\"\npurpose = \"fixture\"\n{extra}\n"
        ));
    }

    /// Declares a reference at `https://github.com/<account>/<name>`; `extra`
    /// is more TOML (`branch`, `pinned`, `shallow`, `sparse`).
    pub fn declare_reference(&mut self, key: &str, account: &str, name: &str, extra: &str) {
        self.tables.push(format!(
            "[references.{key}]\nurl = \"https://github.com/{account}/{name}\"\n\
             purpose = \"fixture\"\n{extra}\n"
        ));
    }

    /// The generated registry document.
    pub fn registry_toml(&self) -> String {
        let mut out = format!("owners = [\"{OWNER}\"]\n\n");
        for table in &self.tables {
            let _ = writeln!(out, "{table}");
        }
        out
    }

    /// Writes the registry at the workspace root.
    pub fn write_registry(&self) -> PathBuf {
        let path = self.root().join(REGISTRY_FILE);
        std::fs::write(&path, self.registry_toml()).unwrap();
        path
    }

    /// Writes the registry and loads its entries the way the binary does:
    /// found by walking up from the root.
    pub fn entries(&self) -> Vec<Entry> {
        self.write_registry();
        let loc = find_registry(&self.root(), None, None, &self.runner()).unwrap();
        assert_eq!(loc.root, self.root());
        ValidRegistry::load(&loc.path).unwrap().entries()
    }

    // --- running the tool ---

    /// `status` over every entry, local refs only.
    pub fn status(&self) -> Vec<EntryStatus> {
        self.status_at(&self.root(), false)
    }

    /// `status --fetch` over every entry.
    pub fn status_with_fetch(&self) -> Vec<EntryStatus> {
        self.status_at(&self.root(), true)
    }

    /// `status` over every entry with `root` as the workspace root — another
    /// path to the same dir, such as a symlink to it.
    pub fn status_at(&self, root: &Path, fetch: bool) -> Vec<EntryStatus> {
        self.status_with(root, fetch, &self.runner(), &self.visibility_base())
    }

    /// `status` over every entry with `root` as the workspace root, run by
    /// `git`, the visibility check reading repos under `visibility_base`.
    pub fn status_with(
        &self,
        root: &Path,
        fetch: bool,
        git: &Git,
        visibility_base: &str,
    ) -> Vec<EntryStatus> {
        let entries = self.entries();
        let run = status(
            &entries,
            &RegistryDirs::new(root, &entries),
            root,
            git,
            StatusOptions {
                fetch,
                jobs: 4,
                visibility_base: Some(visibility_base),
            },
        );
        run.entries
    }

    /// Where the visibility check reads repos by default: a `file://` dir
    /// under the tempdir, `anon/<account>/<name>`, which holds nothing until
    /// a test puts a repo there (`publish_anonymously`) — so no check ever
    /// leaves the machine.
    pub fn visibility_base(&self) -> String {
        format!("file://{}/", self.anonymous_dir().display())
    }

    /// The dir `visibility_base` names.
    pub fn anonymous_dir(&self) -> PathBuf {
        self.base.as_path().join("anon")
    }

    /// Makes `name`'s bare remote readable where the visibility check looks,
    /// as `anon/<OWNER>/<name>`: a repo anyone can read.
    pub fn publish_anonymously(&self, name: &str) {
        let dir = self.anonymous_dir().join(OWNER);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(self.bare(name), dir.join(name)).unwrap();
        // what the check will read: the bare remote's HEAD
        let url = format!("{}{OWNER}/{name}", self.visibility_base());
        assert!(
            self.git(self.base(), &["ls-remote", &url, "HEAD"])
                .ends_with("\tHEAD")
        );
    }

    /// The unregistered scan over the workspace root, with the registry's
    /// entries and owners as the binary loads them.
    pub fn unregistered(&self) -> Vec<UnregisteredClone> {
        let entries = self.entries();
        let registry = ValidRegistry::load(&self.root().join(REGISTRY_FILE)).unwrap();
        scan_unregistered(&self.root(), &entries, registry.owners(), &self.runner())
            .unwrap()
            .unregistered
    }

    /// One entry's status, from a run over every entry.
    pub fn entry(&self, key: &str) -> EntryStatus {
        take_entry(self.status(), key)
    }

    // --- setup assertions ---

    /// Whether `rev` resolves in `repo`.
    pub fn has_ref(&self, repo: &Path, rev: &str) -> bool {
        self.git_output(repo, &["rev-parse", "--verify", "-q", rev])
            .status
            .success()
    }

    /// Asserts `%(upstream:track)` for a local branch: `""` (even or no
    /// upstream), `"[ahead 1]"`, `"[gone]"`, ….
    pub fn assert_track(&self, repo: &Path, branch: &str, track: &str) {
        let got = self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(upstream:track)",
                &format!("refs/heads/{branch}"),
            ],
        );
        assert_eq!(got, track, "track of {branch} in {}", repo.display());
    }

    /// Asserts a local branch's resolved upstream (`""` for none).
    pub fn assert_upstream(&self, repo: &Path, branch: &str, upstream: &str) {
        let got = self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(upstream)",
                &format!("refs/heads/{branch}"),
            ],
        );
        assert_eq!(got, upstream, "upstream of {branch} in {}", repo.display());
    }

    /// Asserts `rev-list --count <args>`.
    pub fn assert_count(&self, repo: &Path, args: &[&str], n: u32) {
        let mut all = vec!["rev-list", "--count"];
        all.extend(args);
        let got: u32 = self.git(repo, &all).parse().unwrap();
        assert_eq!(got, n, "rev-list --count {}", args.join(" "));
    }

    /// Asserts the checkout's `status --porcelain` lines, sorted.
    pub fn assert_porcelain(&self, repo: &Path, lines: &[&str]) {
        let out = self.git_raw(repo, &["status", "--porcelain", "--untracked-files=normal"]);
        let mut got: Vec<&str> = out.lines().collect();
        got.sort_unstable();
        let mut want = lines.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "status of {}", repo.display());
    }

    /// Asserts the checkout is clean.
    pub fn assert_clean(&self, repo: &Path) {
        self.assert_porcelain(repo, &[]);
    }

    /// Asserts whether the repo is shallow.
    pub fn assert_shallow(&self, repo: &Path, shallow: bool) {
        let got = self.git(repo, &["rev-parse", "--is-shallow-repository"]);
        assert_eq!(got, shallow.to_string(), "shallow {}", repo.display());
    }

    /// Asserts which branch HEAD is on (`None` when detached).
    pub fn assert_head(&self, repo: &Path, branch: Option<&str>) {
        let out = self.git_output(repo, &["symbolic-ref", "-q", "--short", "HEAD"]);
        let got = out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        assert_eq!(got.as_deref(), branch, "HEAD of {}", repo.display());
    }
}

/// The `origin` an owned entry expects: SSH.
pub fn owned_origin(name: &str) -> String {
    format!("git@github.com:{OWNER}/{name}")
}

/// The `origin` a third-party reference expects: HTTPS.
pub fn third_party_origin(name: &str) -> String {
    format!("https://github.com/{THIRD_PARTY}/{name}")
}

/// Writes a file under `dir`, creating parents.
pub fn write(dir: &Path, path: &str, content: &str) {
    let path = dir.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Writes an executable file.
pub fn write_executable(dir: &Path, path: &str, content: &str) {
    write(dir, path, content);
    let path = dir.join(path);
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// The entry keyed `key`, from a status run.
pub fn take_entry(entries: Vec<EntryStatus>, key: &str) -> EntryStatus {
    entries
        .into_iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no entry `{key}` in the report"))
}

/// The entry keyed `key`.
pub fn find_entry<'a>(entries: &'a [EntryStatus], key: &str) -> &'a EntryStatus {
    entries
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no entry `{key}` in the report"))
}

/// An entry's unprobed worktrees, as the probe's facts (without what
/// `classify` decided about pruning them).
pub fn unprobed_facts(entry: &EntryStatus) -> Vec<UnprobedWorktree> {
    entry
        .unprobed_worktrees
        .iter()
        .map(|u| u.worktree.clone())
        .collect()
}

/// The branch named `name` in an entry's report.
pub fn branch<'a>(entry: &'a EntryStatus, name: &str) -> &'a BranchStatus {
    entry
        .branches
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no branch `{name}` in {}: {:#?}", entry.key, entry.branches))
}

/// The branch names an entry reports, sorted.
pub fn branch_names(entry: &EntryStatus) -> Vec<&str> {
    let mut names: Vec<&str> = entry.branches.iter().map(|b| b.name.as_str()).collect();
    names.sort_unstable();
    names
}

/// Every file and dir under a git dir with its length, mtime, and inode —
/// compared around a tool call to show it wrote nothing there. The inode
/// catches a same-length rewrite within one timestamp tick: git writes
/// through a lock file renamed into place.
pub type GitDirSnapshot = BTreeMap<PathBuf, (u64, SystemTime, u64)>;

/// Snapshots `git_dir` (a `.git`, or a linked worktree's
/// `.git/worktrees/<name>`), recursively.
pub fn snapshot_git_dir(git_dir: &Path) -> GitDirSnapshot {
    fn walk(root: &Path, dir: &Path, out: &mut GitDirSnapshot) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (meta.len(), meta.modified().unwrap(), meta.ino()),
            );
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = GitDirSnapshot::new();
    walk(git_dir, git_dir, &mut out);
    out
}

/// Asserts two snapshots of one git dir are equal, naming what changed.
pub fn assert_git_dir_unchanged(before: &GitDirSnapshot, after: &GitDirSnapshot) {
    let changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(changed.is_empty(), "the git dir changed: {changed:?}");
}

/// Restores a path's permissions on drop, so the tempdir can be deleted
/// whether or not the test passes.
#[derive(Debug)]
pub struct Unseal(pub PathBuf);

impl Drop for Unseal {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Sets `path`'s mode, restored on the guard's drop; `None` (already
/// restored) when permissions don't bind this user (root), so the caller
/// skips its test.
pub fn seal(path: &Path, mode: u32) -> Option<Unseal> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    let guard = Unseal(path.to_owned());
    let binds = if path.is_dir() {
        std::fs::read_dir(path).is_err()
    } else {
        std::fs::File::open(path).is_err()
    };
    if binds {
        Some(guard)
    } else {
        eprintln!("skipped: permissions don't bind this user (root)");
        None
    }
}

/// Sets a file's mtime, so its stat info no longer matches the index.
pub fn set_mtime(path: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}
