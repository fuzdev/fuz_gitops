//! A rebase's steps past its re-checks: what `git replay` printed, the
//! chain it made, and the move of the branch to it — each driven directly,
//! since the races and the git outputs they catch fall between a re-check
//! and git's write, which no seam in `sync` reaches.

use std::path::{Path, PathBuf};

use super::super::tests::{Repo, step, write_executable};
use super::*;
use crate::git::Git;

/// `main` with two commits `onto` doesn't have: `(onto, from)`.
fn diverged(repo: &Repo) -> (String, String) {
    let base = repo.git(&["rev-parse", "main"]);
    repo.commit("local-1");
    let from = repo.commit("local-2");
    let onto = repo.child_of(&base);
    (onto, from)
}

/// A runner whose `git replay --onto …` prints `fake` (when it exists) and
/// exits `0`, writing nothing; every other call, `replay -h` among them, is
/// the real git's.
fn faking_replay(repo: &Repo, fake: &Path) -> Git {
    let bin = repo.tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real_path = std::env::var("PATH").unwrap();
    let wrapper = format!(
        "#!/bin/sh\ncase \" $* \" in *\" replay \"*\"--onto \"*) if [ -f '{fake}' ]; then cat '{fake}'; \
         exit 0; fi;; esac\nPATH='{real_path}' exec git \"$@\"\n",
        fake = fake.display()
    );
    write_executable(&bin.join("git"), &wrapper);
    let mut env = repo.env.clone();
    env.retain(|(k, _)| k != "PATH");
    env.push((
        "PATH".into(),
        format!("{}:{real_path}", bin.display()).into(),
    ));
    Git::with_clean_env(env)
}

#[test]
fn a_replay_of_a_branch_moved_since_classifying_is_held() {
    let repo = Repo::new();
    let (onto, from) = diverged(&repo);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);

    // the commit classified, `from`, is no longer the branch's: the update
    // git prints names the commit it read
    let classified = repo.git(&["rev-parse", "main~1"]);
    assert!(matches!(
        step.replay(&repo.dir, &onto, &classified),
        Ok(Replayed::Moved)
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), from);

    // the control: the commit classified, replayed, nothing moved
    let Ok(Replayed::Tip(to)) = step.replay(&repo.dir, &onto, &from) else {
        panic!("the replay of the branch as classified");
    };
    assert_ne!(to, from);
    assert_eq!(repo.git(&["rev-parse", "main"]), from);
    assert_eq!(repo.git(&["rev-parse", &format!("{to}~2")]), onto);
}

#[test]
fn a_replay_of_a_branch_moved_after_git_read_it_is_held() {
    let repo = Repo::new();
    let (onto, from) = diverged(&repo);
    let fake = repo.tmp.path().join("replay.out");
    let git = faking_replay(&repo, &fake);
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);
    let new = "1".repeat(40);
    std::fs::write(&fake, format!("update refs/heads/main {new} {from}\n")).unwrap();

    // the control: git read the commit classified, and the branch still
    // holds it
    assert!(matches!(
        step.replay(&repo.dir, &onto, &from),
        Ok(Replayed::Tip(to)) if to == new
    ));

    // the update git prints names the commit classified, but the branch
    // moved after git read it
    let elsewhere = repo.git(&["rev-parse", "main~1"]);
    repo.git(&["update-ref", "refs/heads/main", &elsewhere]);
    assert!(matches!(
        step.replay(&repo.dir, &onto, &from),
        Ok(Replayed::Moved)
    ));
}

#[test]
fn a_replay_printing_anything_but_the_branchs_one_update_fails() {
    let repo = Repo::new();
    let (onto, from) = diverged(&repo);
    let fake = repo.tmp.path().join("replay.out");
    let git = faking_replay(&repo, &fake);
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);
    let printing = |out: &str| {
        std::fs::write(&fake, out).unwrap();
        match step.replay(&repo.dir, &onto, &from) {
            Err(message) => message,
            Ok(_) => panic!("accepted {out:?}"),
        }
    };
    let new = "1".repeat(40);

    assert_eq!(
        printing(""),
        "git replay printed no ref update for refs/heads/main"
    );
    assert_eq!(
        printing(&format!(
            "update refs/heads/main {new} {from}\nupdate refs/heads/main {new} {from}\n"
        )),
        format!(
            "git replay printed no single ref update for refs/heads/main: update \
             refs/heads/main {new} {from}\nupdate refs/heads/main {new} {from}"
        )
    );
    assert_eq!(
        printing(&format!("create refs/heads/main {new}\n")),
        format!("git replay printed `create refs/heads/main {new}`")
    );
    assert_eq!(
        printing(&format!("update refs/heads/other {new} {from}\n")),
        "git replay updated refs/heads/other, not refs/heads/main"
    );
    // nothing printed, and the branch elsewhere: git moved it itself
    let elsewhere = repo.git(&["rev-parse", "main~1"]);
    repo.git(&["update-ref", "refs/heads/main", &elsewhere]);
    let message = printing("");
    assert!(
        message.starts_with(&format!(
            "git replay moved refs/heads/main itself, from {from} to {elsewhere}"
        )),
        "{message}"
    );
}

#[test]
fn a_replayed_chain_unlike_the_original_fails() {
    let repo = Repo::new();
    let onto = repo.git(&["rev-parse", "main"]);
    let a = repo.child_of(&onto);
    let b = repo.child_of(&a);
    let from = repo.child_of(&b);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);

    // fewer commits replayed than the originals
    let short = repo.child_of(&onto);
    assert_eq!(
        step.already_upstream(&repo.dir, &onto, &from, &short)
            .unwrap_err(),
        "git replay made 1 commits of 3"
    );
    // as many, one of them a merge: reached past the links before it only
    // because `child_of`'s originals change nothing, so an empty replayed
    // link is no commit already upstream
    let tree = format!("{onto}^{{tree}}");
    let left = repo.git(&["commit-tree", &tree, "-p", &onto, "-m", "left"]);
    let right = repo.git(&["commit-tree", &tree, "-p", &onto, "-m", "right"]);
    let merge = repo.git(&[
        "commit-tree",
        &tree,
        "-p",
        &left,
        "-p",
        &right,
        "-m",
        "merge",
    ]);
    assert_eq!(
        step.already_upstream(&repo.dir, &onto, &from, &merge)
            .unwrap_err(),
        format!("git replay made {merge} no single parent")
    );
}

#[test]
fn an_in_place_move_is_held_unless_the_branch_is_where_and_what_it_was() {
    let repo = Repo::new();
    let (onto, from) = diverged(&repo);
    let to = repo.child_of(&onto);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let held = |done: Result<UpdateDone, String>| {
        matches!(done, Ok(UpdateDone::Held(BranchSyncHold::Changed)))
    };

    // checked out here now
    assert!(held(step(&git, "main", &common_dir).replayed_in_place(
        &repo.dir,
        from.clone(),
        to.clone()
    )));
    assert_eq!(repo.git(&["rev-parse", "main"]), from);

    repo.git(&["switch", "-q", "--detach"]);
    // a symbolic ref now, which the write would go through
    repo.git(&["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    assert!(held(step(&git, "alias", &common_dir).replayed_in_place(
        &repo.dir,
        from.clone(),
        to.clone()
    )));
    assert_eq!(repo.git(&["rev-parse", "main"]), from);
    repo.git(&["symbolic-ref", "-d", "refs/heads/alias"]);

    let step = step(&git, "main", &common_dir);
    // moved by another hand: the swap fails, the other hand's commit stands
    let moved = repo.git(&["rev-parse", "main~1"]);
    repo.git(&["update-ref", "refs/heads/main", &moved]);
    assert!(held(step.replayed_in_place(
        &repo.dir,
        from.clone(),
        to.clone()
    )));
    assert_eq!(repo.git(&["rev-parse", "main"]), moved);
    repo.git(&["update-ref", "refs/heads/main", &from]);

    // the swap failing on the branch as it was is git's failure, said
    let missing = "1".repeat(40);
    let failed = step.replayed_in_place(&repo.dir, from.clone(), missing);
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(repo.git(&["rev-parse", "main"]), from);

    // the control: moved, and the reflog written though the repo keeps
    // none
    assert!(matches!(
        step.replayed_in_place(&repo.dir, from.clone(), to.clone()),
        Ok(UpdateDone::Updated { .. })
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), to);
    assert_eq!(repo.git(&["rev-parse", "main@{1}"]), from);
    assert_eq!(
        repo.git(&["log", "-g", "-1", "--format=%gs", "refs/heads/main", "--"]),
        "repos: rebase onto the fetched tip"
    );
}

#[test]
fn a_move_in_the_checkout_reads_it_again_after_the_replay() {
    let repo = Repo::new();
    let (onto, from) = diverged(&repo);
    let to = repo.child_of(&onto);
    let git = repo.runner();
    let common_dir = repo.dir.join(".git");
    let step = step(&git, "main", &common_dir);
    let checkout: PathBuf = repo.dir.clone();

    // dirtied since the rebase's first read
    std::fs::write(checkout.join("scratch.txt"), "mine").unwrap();
    assert!(matches!(
        step.replayed_in_checkout(&checkout, from.clone(), to.clone()),
        Ok(UpdateDone::Held(BranchSyncHold::DirtyCheckout))
    ));
    std::fs::remove_file(checkout.join("scratch.txt")).unwrap();
    assert_eq!(repo.git(&["rev-parse", "main"]), from);

    // a commit landed on the branch since the replay
    let landed = repo.commit("landed");
    assert!(matches!(
        step.replayed_in_checkout(&checkout, from.clone(), to.clone()),
        Ok(UpdateDone::Held(BranchSyncHold::Changed))
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), landed);

    // the control: clean, the branch where the replay read it, moved
    repo.git(&["reset", "-q", "--hard", &from]);
    assert!(matches!(
        step.replayed_in_checkout(&checkout, from, to.clone()),
        Ok(UpdateDone::Updated { .. })
    ));
    assert_eq!(repo.git(&["rev-parse", "main"]), to);
}
