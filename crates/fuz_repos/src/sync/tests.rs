use super::step::{LazyFetch, PushedRef, first_message, pushed_ref, rejected};
use super::*;
use crate::classify::lazy_transport;
use crate::porcelain::{self, ConfigFacts};
use crate::registry::RepoUrl;

#[test]
fn git_s_first_error_line_is_the_message() {
    assert_eq!(
        first_message(
            "Updating a..b\nerror: Your local changes to the following files would be \
             overwritten by merge:\n\tf\nAborting\n"
        ),
        "error: Your local changes to the following files would be overwritten by merge:"
    );
    assert_eq!(first_message("\n  hint: x\n"), "hint: x");
    assert_eq!(first_message(""), "git failed without a message");
}

#[test]
fn a_push_is_read_from_its_status_line() {
    let dst = "refs/heads/main";
    // as git 2.47 prints them: every remote ref, each not pushed `no match`
    let out = "error refs/heads/feat no match\nok refs/heads/main\nerror refs/tags/v1 no match\n";
    assert_eq!(
        pushed_ref(out, dst),
        Some(PushedRef {
            ok: true,
            message: None
        })
    );
    assert_eq!(
        pushed_ref("ok refs/heads/main up to date\n", dst),
        Some(PushedRef {
            ok: true,
            message: Some("up to date")
        })
    );
    assert_eq!(
        pushed_ref("error refs/heads/main stale info\n", dst),
        Some(PushedRef {
            ok: false,
            message: Some("stale info")
        })
    );
    // a C-quoted message loses its quotes
    assert_eq!(
        pushed_ref("error refs/heads/main \"hook \\\"x\\\" declined\"\n", dst)
            .and_then(|p| p.message),
        Some("hook \\\"x\\\" declined")
    );
    // another ref's line, a prefix of it, or none, is no answer
    assert_eq!(pushed_ref(out, "refs/heads/mai"), None);
    assert_eq!(pushed_ref("ok refs/heads/main/x\n", dst), None);
    assert_eq!(pushed_ref("Everything up-to-date\n", dst), None);
    assert_eq!(pushed_ref("", dst), None);
}

#[test]
fn a_rejected_push_is_held_or_failed_by_why() {
    // the remote isn't at the fetched tip: rerun
    for why in ["stale info", "fetch first", "non-fast forward"] {
        assert!(
            matches!(rejected(why, ""), Stop::Held(SyncHold::Changed)),
            "{why}"
        );
    }
    // the remote's refusal, with its own words
    let stderr = "remote: error: GH006: Protected branch update failed for refs/heads/main.   \n\
                  remote: error: Changes must be made through a pull request.   \n\
                  error: failed to push some refs to 'github.com:me/app'\n";
    assert!(matches!(
        rejected("protected branch hook declined", stderr),
        Stop::PushFailed(RemoteFailure::Rejected { reason, message })
            if reason == "protected branch hook declined"
                && message.as_deref()
                    == Some("GH006: Protected branch update failed for refs/heads/main.")
    ));
    // a host refusing non-fast-forwards words it with a hyphen, as its own
    assert!(matches!(
        rejected("non-fast-forward", "remote: error: denying non-fast-forward\n"),
        Stop::PushFailed(RemoteFailure::Rejected { reason, .. }) if reason == "non-fast-forward"
    ));
    assert!(matches!(
        rejected("hook declined", "remote: nope\n"),
        Stop::PushFailed(RemoteFailure::Rejected { message: Some(m), .. }) if m == "nope"
    ));
    assert!(matches!(
        rejected("hook declined", ""),
        Stop::PushFailed(RemoteFailure::Rejected { message: None, .. })
    ));
    // git's other refusals fail with its words
    for why in ["needs force", "no match", "expecting report"] {
        assert!(matches!(
            rejected(why, ""),
            Stop::PushFailed(RemoteFailure::Failed { message })
                if message == format!("rejected: {why}")
        ));
    }
    assert!(matches!(
        rejected("", ""),
        Stop::PushFailed(RemoteFailure::Failed { message }) if message == "rejected: no reason"
    ));
}

/// `main` of `/ws/app`, checked out and a commit ahead, as classify
/// would read it, its verdict a push; no branch probed, so past the
/// guards the push would fail on it.
fn ahead_main() -> (RepoFacts, BranchStatus) {
    let facts = RepoFacts {
        path: "/ws/app".into(),
        common_dir: PathBuf::from("/ws/app/.git"),
        config: ConfigFacts::default(),
        status: porcelain::StatusFacts {
            head: Head::Branch {
                name: "main".into(),
            },
            uncommitted: crate::state::Uncommitted::default(),
            stashes: 0,
        },
        in_progress: None,
        primary_linked: false,
        primary_locked: false,
        locks: Vec::new(),
        worktrees: Vec::new(),
        registry_worktrees: std::collections::HashSet::new(),
        unprobed: Vec::new(),
        unreadable: Vec::new(),
        relative_gitdir: None,
        git_dirs: Vec::new(),
        bare_main: None,
        branches: Vec::new(),
        layout: crate::state::Layout {
            shallow: false,
            sparse: false,
            partial_filter: None,
        },
        fetched_at: None,
        fetch_failed: false,
        push_urls: Some(vec!["git@github.com:me/app".into()]),
    };
    let b = BranchStatus {
        name: "main".into(),
        upstream: Some("origin/main".into()),
        worktree: Some("/ws/app".into()),
        symref: None,
        unique_commits: 1,
        newest_commit_at: 0,
        relation: crate::state::Relation::Ahead { commits: 1 },
        verdict: Verdict::Act {
            action: SyncAction::Push { commits: 1 },
        },
    };
    (facts, b)
}

/// The act-time third-party guard, driven directly: classify reads a
/// third-party reference's branch ahead as local-only work, never a
/// push, and the guard fails one that ever slips, before anything is
/// read.
#[test]
fn a_third_party_push_fails_at_act_time_whatever_the_verdict() {
    let git = Git::with_clean_env(Vec::new());
    let read_live = || -> LiveSessions { panic!("the guard reads no sessions") };
    let lib = Entry {
        key: "lib".into(),
        kind: crate::registry::EntryKind::Reference,
        dir: "lib".into(),
        url: RepoUrl::try_from("https://github.com/them/lib".to_owned()).unwrap(),
        writable: false,
        archived: false,
        visibility: None,
        ci: false,
        branch: None,
        pinned: false,
        shallow: false,
        sparse: None,
        same_repo_as: None,
    };
    let entries = [lib];
    let actor = Actor {
        git: &git,
        root: Path::new("/ws"),
        entries: &entries,
        checkouts: &[],
        read_live: &read_live,
    };
    let (facts, b) = ahead_main();
    let action = SyncAction::Push { commits: 1 };
    assert_eq!(
        actor.act(0, &facts, &b, action),
        BranchOutcome::Failed {
            action,
            message: "main is a third-party reference's, which is never pushed".into(),
        }
    );
}

// the checks after the fact, driven from just past the re-checks: the
// races they catch fall between a re-check and git's write, which no
// seam in `sync` reaches

/// A repo in a tempdir, no global or system config, reflogs off — so
/// what a branch's reflog holds, sync wrote.
struct Repo {
    tmp: tempfile::TempDir,
    dir: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl Repo {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("app");
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = vec![
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_AUTHOR_NAME".into(), "a".into()),
            ("GIT_AUTHOR_EMAIL".into(), "a@example.com".into()),
            ("GIT_COMMITTER_NAME".into(), "a".into()),
            ("GIT_COMMITTER_EMAIL".into(), "a@example.com".into()),
        ];
        env.extend(std::env::var_os("PATH").map(|p| ("PATH".into(), p)));
        let repo = Self { tmp, dir, env };
        std::fs::create_dir(&repo.dir).unwrap();
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "core.logAllRefUpdates", "false"]);
        repo.commit("root");
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Commits a new file `name` on HEAD.
    fn commit(&self, name: &str) -> String {
        std::fs::write(self.dir.join(name), name).unwrap();
        self.git(&["add", name]);
        self.git(&["commit", "-q", "-m", name]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// A commit on `parent`'s tree with `parent` as its parent, made
    /// without touching HEAD or the files.
    fn child_of(&self, parent: &str) -> String {
        let tree = format!("{parent}^{{tree}}");
        self.git(&["commit-tree", &tree, "-p", parent, "-m", "upstream"])
    }

    fn runner(&self) -> Git {
        Git::with_clean_env(self.env.clone())
    }
}

/// Writes an executable at `path` from a child process, so this one
/// never holds the file open for writing. Git's own spawns from other
/// test threads fork this process: a fork taken while a write fd is
/// open here keeps a copy of it until the child execs (`O_CLOEXEC`
/// closes it only then), and an exec of the file meanwhile fails with
/// `ETXTBSY` ("Text file busy"). The child's fds are its own.
fn write_executable(path: &Path, content: &str) {
    use std::io::Write as _;
    let mut child = std::process::Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "writing {}",
        path.display()
    );
}

fn step<'a>(git: &'a Git, branch: &'a str, common_dir: &'a Path) -> Step<'a> {
    Step::new(git, Path::new("/"), branch, common_dir, None)
}

#[test]
fn a_push_moves_the_remote_tracking_ref_only_from_the_fetched_tip() {
    let repo = Repo::new();
    let fetched = repo.git(&["rev-parse", "main"]);
    let pushed = repo.child_of(&fetched);
    let meanwhile = repo.child_of(&fetched);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let tracking = "refs/remotes/origin/main";
    let step = step(&git, "main", &common_dir);

    // a fetch moved it after the push: its value stands
    repo.git(&["update-ref", tracking, &meanwhile]);
    step.record_push(&repo.dir, tracking, &fetched, &pushed);
    assert_eq!(repo.git(&["rev-parse", tracking]), meanwhile);
    // at the fetched tip: moved, as `git push` records a push
    repo.git(&["update-ref", tracking, &fetched]);
    step.record_push(&repo.dir, tracking, &fetched, &pushed);
    assert_eq!(repo.git(&["rev-parse", tracking]), pushed);
    // never a ref outside origin's remote-tracking refs
    step.record_push(&repo.dir, "refs/heads/main", &fetched, &pushed);
    assert_eq!(repo.git(&["rev-parse", "main"]), fetched);
}

#[test]
fn a_commit_the_switch_dropped_fails_the_move() {
    let repo = Repo::new();
    let counted = repo.git(&["rev-parse", "main"]);
    // landed after the re-count
    let landed = repo.commit("landed");
    let tip = repo.child_of(&counted);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);

    let Err(message) = step.switch_reset(&repo.dir, counted, tip.clone()) else {
        panic!("the dropped commit went unreported");
    };
    assert!(message.contains(&landed), "{message}");
    assert!(message.contains("git reflog refs/heads/main"), "{message}");
    // moved all the same, and the reflog holds what it dropped
    assert_eq!(repo.git(&["rev-parse", "main"]), tip);
    assert_eq!(repo.git(&["rev-parse", "main@{1}"]), landed);

    // the control: at the commit counted, the move stands
    let next = repo.child_of(&tip);
    assert!(matches!(
        step.switch_reset(&repo.dir, tip.clone(), next.clone()),
        Ok(UpdateDone::Updated { from, to }) if from == tip && to == next
    ));
}

#[test]
fn a_head_switched_before_the_merge_fails_the_fast_forward() {
    let repo = Repo::new();
    let from = repo.git(&["rev-parse", "main"]);
    let tip = repo.child_of(&from);
    repo.git(&["branch", "feat"]);
    // switched away after the status read it on `feat`
    repo.git(&["switch", "-q", "-c", "other"]);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "feat", &common_dir);

    let Err(message) = step.merge_ff(&repo.dir, from.clone(), tip.clone()) else {
        panic!("the merge's move of another branch went unreported");
    };
    assert!(
        message.contains(&format!(
            "HEAD was on refs/heads/other when the merge ran; refs/heads/feat is at \
             {from}, not {tip}"
        )),
        "{message}"
    );
    // the merge moved the branch HEAD was on, by a fast-forward
    assert_eq!(repo.git(&["rev-parse", "other"]), tip);
    assert_eq!(repo.git(&["rev-parse", "feat"]), from);

    // the control: on the branch, the fast-forward stands
    repo.git(&["switch", "-q", "feat"]);
    assert!(matches!(
        step.merge_ff(&repo.dir, from.clone(), tip.clone()),
        Ok(UpdateDone::Updated { from: f, to }) if f == from && to == tip
    ));
}

#[test]
fn a_branch_moved_past_the_tip_before_the_merge_is_held() {
    let repo = Repo::new();
    let from = repo.git(&["rev-parse", "main"]);
    let tip = repo.child_of(&from);
    // another hand fast-forwards past the tip after the status read it
    let past = repo.child_of(&tip);
    repo.git(&["merge", "-q", "--ff-only", &past]);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);

    // git's "Already up to date", and nothing lost
    assert!(matches!(
        step.merge_ff(&repo.dir, from, tip),
        Ok(UpdateDone::Held(SyncHold::Changed))
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), past);
}

#[test]
fn a_branch_moved_to_the_tip_by_another_hand_is_held_not_moved() {
    // with no reflog: the switch finds the branch there and writes none
    let repo = Repo::new();
    let counted = repo.git(&["rev-parse", "main"]);
    let tip = repo.child_of(&counted);
    repo.git(&["update-ref", "refs/heads/main", &tip]);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);

    assert!(matches!(
        step.switch_reset(&repo.dir, counted, tip.clone()),
        Ok(UpdateDone::Held(SyncHold::Changed))
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), tip);

    // with one: its newest entry is the other hand's, whose previous
    // value is the commit counted, so it would pass for the switch's
    let next = repo.child_of(&tip);
    repo.git(&[
        "update-ref",
        "--create-reflog",
        "-m",
        "another hand",
        "refs/heads/main",
        &next,
    ]);
    assert_eq!(repo.git(&["rev-parse", "main@{1}"]), tip);
    assert!(matches!(
        step.switch_reset(&repo.dir, tip, next.clone()),
        Ok(UpdateDone::Held(SyncHold::Changed))
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), next);
}

// the lazy fetch's scope: one transport, origin's own, and the
// checkout's calls alone

#[test]
fn a_lazy_fetch_takes_origins_own_transport_alone() {
    for (origin, want) in [
        ("git@github.com:me/wpt", Some("ssh")),
        ("ssh://git@github.com/me/wpt", Some("ssh")),
        ("git+ssh://github.com/me/wpt", Some("ssh")),
        ("https://github.com/them/lib", Some("https")),
        // an owned repo whose origin is HTTPS: over HTTPS, never SSH
        ("https://github.com/me/wpt", Some("https")),
        ("http://github.com/them/lib", None),
        ("git://github.com/them/lib", None),
        ("file:///srv/lib.git", None),
        ("/srv/lib.git", None),
        ("ext::sh -c touch% /tmp/x", None),
        ("https://github.com/them/%6Cib", None),
    ] {
        assert_eq!(lazy_transport(origin), want, "{origin}");
    }
}

#[test]
fn a_lazy_fetch_is_a_partial_clones_with_origin_its_one_promisor() {
    let repo = RepoUrl::try_from("https://github.com/me/wpt".to_owned()).unwrap();
    let lazy_fetch = |config: &ConfigFacts, env_ssh| lazy_fetch(config, env_ssh, &repo);
    // origin as configured, and as git resolves it
    let partial = |origin: Option<&str>| ConfigFacts {
        origin_urls: origin.map(porcelain::OriginUrl::repo).into_iter().collect(),
        origin_fetch_url: origin.map(str::to_owned),
        partial_filter: Some("blob:none".into()),
        ..ConfigFacts::default()
    };
    let https = partial(Some("https://github.com/me/wpt"));
    assert_eq!(
        lazy_fetch(&https, false),
        Some(LazyFetch {
            transport: "https",
            batch_ssh: true,
            repo: &repo,
        })
    );
    let ssh = partial(Some("git@github.com:them/lib"));
    assert_eq!(
        lazy_fetch(&ssh, false),
        Some(LazyFetch {
            transport: "ssh",
            batch_ssh: true,
            repo: &repo,
        })
    );
    // the user's SSH, left alone
    assert_eq!(lazy_fetch(&ssh, true).map(|l| l.batch_ssh), Some(false));
    // the transport is where git connects, a rewrite applied: SSH
    // rewritten to HTTPS fetches over HTTPS, and the other way
    let rewritten = ConfigFacts {
        origin_fetch_url: Some("https://github.com/me/wpt".into()),
        ..ssh.clone()
    };
    assert_eq!(
        lazy_fetch(&rewritten, false).map(|l| l.transport),
        Some("https")
    );
    let rewritten = ConfigFacts {
        origin_fetch_url: Some("git@github.com:me/wpt".into()),
        ..https.clone()
    };
    assert_eq!(
        lazy_fetch(&rewritten, false).map(|l| l.transport),
        Some("ssh")
    );
    // unread, or rewritten to a transport the fetch may not take
    let unread = ConfigFacts {
        origin_fetch_url: None,
        ..https.clone()
    };
    let to_file = ConfigFacts {
        origin_fetch_url: Some("file:///srv/wpt.git".into()),
        ..https
    };
    let configured = ConfigFacts {
        ssh_command: true,
        ..ssh.clone()
    };
    assert_eq!(
        lazy_fetch(&configured, false).map(|l| l.batch_ssh),
        Some(false)
    );
    // none: not partial, another promisor, no origin URL, or one naming
    // no transport the fetch may take
    let whole = ConfigFacts {
        partial_filter: None,
        ..ssh.clone()
    };
    let other = ConfigFacts {
        other_promisor: true,
        ..ssh
    };
    for config in [
        whole,
        other,
        partial(None),
        partial(Some("file:///srv/wpt.git")),
        unread,
        to_file,
    ] {
        assert_eq!(lazy_fetch(&config, false), None, "{config:?}");
    }
}

/// Every action's git calls, through a `git` that logs what each saw:
/// lazy fetching is lifted — over the lazy fetch's one transport — in
/// the calls that rewrite a working tree (`merge`, `switch`) and no
/// other, however the step was made.
#[test]
fn only_a_checkouts_calls_lift_lazy_fetching() {
    let repo = Repo::new();
    let root = repo.git(&["rev-parse", "main"]);
    let tip = repo.child_of(&root);
    let side = repo.child_of(&tip);
    for (name, at) in [
        ("main", &tip),
        ("feat", &tip),
        ("old", &side),
        ("side", &side),
    ] {
        repo.git(&["update-ref", &format!("refs/remotes/origin/{name}"), at]);
    }
    repo.git(&["branch", "feat", &root]);
    repo.git(&["branch", "old", &root]);
    // a `git` first on PATH, logging each call it passes to the real one
    let bin = repo.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = repo.tmp.path().join("calls.log");
    let real_path = std::env::var("PATH").unwrap();
    let wrapper = format!(
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"${{GIT_NO_LAZY_FETCH-unset}}\" \
         \"${{GIT_ALLOW_PROTOCOL-unset}}\" \"$*\" >> '{}'\nPATH='{real_path}' exec git \"$@\"\n",
        log.display()
    );
    write_executable(&bin.join("git"), &wrapper);
    let mut env = repo.env.clone();
    env.retain(|(k, _)| k != "PATH");
    env.push((
        "PATH".into(),
        format!("{}:{real_path}", bin.display()).into(),
    ));
    let git = Git::with_clean_env(env);
    let common_dir = repo.dir.join(".git");
    let url = RepoUrl::try_from("https://github.com/me/app".to_owned()).unwrap();
    repo.git(&["remote", "add", "origin", "https://github.com/me/app"]);
    let lazy = Some(LazyFetch {
        transport: "https",
        batch_ssh: false,
        repo: &url,
    });
    let step = |branch| Step::new(&git, repo.tmp.path(), branch, &common_dir, lazy);

    let updated = |done: Result<UpdateDone, String>| matches!(done, Ok(UpdateDone::Updated { .. }));
    assert!(updated(
        step("feat").ff_in_place(&repo.dir, "refs/remotes/origin/feat")
    ));
    assert!(updated(
        step("main").ff_in_checkout(&repo.dir, "refs/remotes/origin/main")
    ));
    assert!(updated(
        step("old").move_in_place(&repo.dir, "refs/remotes/origin/old")
    ));
    let moved = step("main").move_in_checkout(&repo.dir, "refs/remotes/origin/side");
    assert!(matches!(moved, Ok(UpdateDone::Updated { .. })), "{moved:?}");
    let oid = repo.git(&["rev-parse", "main"]);
    // held at its re-checks, past its reads
    let pushed = step("main").push(
        &repo.dir,
        &Push {
            oid: &oid,
            upstream: "refs/remotes/origin/side",
            target: "refs/heads/main",
            commits: 1,
            shallow: false,
            url: &url,
            batch_ssh: false,
        },
    );
    assert!(
        matches!(pushed, Ok(PushDone::Stopped(Stop::Held(_)))),
        "{pushed:?}"
    );

    let calls = std::fs::read_to_string(&log).unwrap();
    let mut checkouts = 0;
    for call in calls.lines() {
        let mut f = call.splitn(3, '|');
        let (lazy, allowed, args) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
        let checkout = [" merge ", " switch "].iter().any(|c| args.contains(c));
        if checkout {
            checkouts += 1;
            assert_eq!((lazy, allowed), ("0", "https"), "{call}");
        } else {
            assert_eq!(lazy, "1", "{call}");
            assert_ne!(allowed, "https", "{call}");
        }
    }
    assert_eq!(checkouts, 2, "{calls}");
}
