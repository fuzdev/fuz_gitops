use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn walks_up_to_the_first_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let nested = root.join("repo/src/lib");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join(REGISTRY_FILE), "owners = []\n").unwrap();

    let git = Git::new();
    let found = find_registry(&nested, None, None, &git).unwrap();
    assert_eq!(found.root, root);
    assert_eq!(found.path, root.join(REGISTRY_FILE));
    // the fallback through git runs only when the walk finds nothing
    assert_eq!(git.spawns(), 0);
}

#[test]
fn a_symlinked_registry_roots_at_the_link() {
    let tmp = tempfile::tempdir().unwrap();
    let meta = tmp.path().join("meta");
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::create_dir_all(ws.join("repo")).unwrap();
    std::fs::write(meta.join(REGISTRY_FILE), "owners = []\n").unwrap();
    std::os::unix::fs::symlink(meta.join(REGISTRY_FILE), ws.join(REGISTRY_FILE)).unwrap();

    let found = find_registry(&ws.join("repo"), None, None, &Git::new()).unwrap();
    assert_eq!(found.root, ws);
}

/// Writes `dir/repos.toml` — the registry for real, or a symlink to
/// `target` — creating `dir`.
fn place(dir: &Path, target: Option<&Path>) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(REGISTRY_FILE);
    match target {
        Some(target) => std::os::unix::fs::symlink(target, &path).unwrap(),
        None => std::fs::write(&path, "owners = []\n").unwrap(),
    }
    path
}

/// Makes `dir` look like a main checkout to discovery: a `.git` dir.
fn checkout(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
}

#[test]
fn a_registry_in_a_checkout_roots_at_the_nearest_link_above_it() {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path();
    let ws = top.join("ws");
    let meta = ws.join("meta");
    // kept in `meta` under `reg/`, linked inside `meta` too; the root's
    // link names that one, and a stray link further out names the root's
    let real = place(&meta.join("reg"), None);
    let inside = place(&meta, Some(Path::new("reg/repos.toml")));
    let link = place(&ws, Some(&inside));
    place(top, Some(&link));
    checkout(&meta);
    for path in [&inside, &link, &top.join(REGISTRY_FILE)] {
        assert_eq!(path.canonicalize().unwrap(), real);
    }
    let deep = meta.join("reg/src/lib");
    std::fs::create_dir_all(&deep).unwrap();

    let git = Git::new();
    for start in [&deep, &meta.join("reg"), &meta, &ws] {
        let found = find_registry(start, None, None, &git).unwrap();
        assert_eq!(found.root, ws, "from {}", start.display());
        assert_eq!(found.path, link);
        assert!(found.discovered);
    }
    // outside any checkout, the first found is the root
    let found = find_registry(top, None, None, &git).unwrap();
    assert_eq!(found.root, top);
    // a `.git` dir: no spawn
    assert_eq!(git.spawns(), 0);

    // not a checkout: `meta`'s own links are where it's found
    std::fs::remove_dir(meta.join(".git")).unwrap();
    let found = find_registry(&deep, None, None, &git).unwrap();
    assert_eq!(found.root, meta.join("reg"));
    let found = find_registry(&meta, None, None, &git).unwrap();
    assert_eq!(found.root, meta);

    // a hard link is the same file too
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let real = place(&ws.join("meta"), None);
    checkout(&ws.join("meta"));
    std::fs::hard_link(&real, ws.join(REGISTRY_FILE)).unwrap();
    let found = find_registry(&ws.join("meta"), None, None, &git).unwrap();
    assert_eq!(found.root, ws);
}

#[test]
fn a_different_registry_further_out_never_captures() {
    let tmp = tempfile::tempdir().unwrap();
    // an outer workspace with a registry of its own, and an inner one
    // whose registry the outer's copies
    let top = tmp.path();
    let outer = top.join("outer");
    let inner = outer.join("inner");
    let real = place(&inner, None);
    std::fs::copy(&real, outer.join(REGISTRY_FILE)).unwrap();
    assert_ne!(file_id(&real), file_id(&outer.join(REGISTRY_FILE)));

    let git = Git::new();
    let found = find_registry(&inner, None, None, &git).unwrap();
    assert_eq!(found.root, inner);
    assert_eq!(found.path, real);
    checkout(&inner);
    let found = find_registry(&inner, None, None, &git).unwrap();
    assert_eq!(found.root, inner);

    // nor does it stop the search: a link to the inner one further out
    // still roots there
    place(top, Some(&real));
    let found = find_registry(&inner, None, None, &git).unwrap();
    assert_eq!(found.root, top);
    assert_eq!(found.path, top.join(REGISTRY_FILE));
    // and from the outer workspace, its own registry is the one
    let found = find_registry(&outer, None, None, &git).unwrap();
    assert_eq!(found.root, outer);
}

#[test]
fn an_ancestor_registry_that_cannot_be_read_is_passed_over() {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path();
    let ws = top.join("ws");
    let meta = ws.join("meta");
    let real = place(&meta, None);
    place(top, Some(&real));
    // between them: a dangling link, a loop, and a link into a dir that
    // can't be searched
    let dangling = place(&ws, Some(&top.join("nowhere")));
    let looped = ws.join("loop");
    place(&looped, Some(&looped.join(REGISTRY_FILE)));
    let sealed = top.join("sealed");
    place(&sealed, Some(&real));
    let blocked = ws.join("blocked");
    place(&blocked, Some(&sealed.join(REGISTRY_FILE)));
    let unreadable = [
        dangling,
        looped.join(REGISTRY_FILE),
        blocked.join(REGISTRY_FILE),
    ];

    let starts = [
        meta.clone(),
        place(&looped.join("meta"), Some(&real)),
        place(&blocked.join("meta"), Some(&real)),
    ]
    .map(|path| path.parent().unwrap().to_owned());
    for start in &starts {
        checkout(start);
    }

    let git = Git::new();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable: Vec<bool> = unreadable
        .iter()
        .map(|path| std::fs::metadata(path).is_ok())
        .collect();
    let found = starts
        .clone()
        .map(|start| find_registry(&start, None, None, &git).unwrap());
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
    // the superuser searches any dir: the sealed one reads then, the same
    // file
    assert!(matches!(readable[..], [false, false, _]), "{readable:?}");
    for (start, found) in starts.iter().zip(found) {
        assert_eq!(found.root, top, "from {}", start.display());
        assert_eq!(found.path, top.join(REGISTRY_FILE));
    }

    // with nothing further out, the registry stays where it's found
    std::fs::remove_file(top.join(REGISTRY_FILE)).unwrap();
    let found = find_registry(&meta, None, None, &git).unwrap();
    assert_eq!(found.root, meta);
}

#[test]
fn the_walk_is_over_the_physical_path() {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().canonicalize().unwrap();
    let ws = top.join("ws");
    let meta = ws.join("meta");
    let real = place(&meta, None);
    checkout(&meta);
    std::fs::create_dir(ws.join("app")).unwrap();
    // `decoy/..` is `ws` to the kernel, `elsewhere` read lexically, where
    // a stray link names the registry
    let elsewhere = top.join("elsewhere");
    place(&elsewhere, Some(&real));
    std::os::unix::fs::symlink(ws.join("app"), elsewhere.join("decoy")).unwrap();
    let dotted = elsewhere.join("decoy/../meta");
    assert_eq!(dotted.canonicalize().unwrap(), meta);

    let git = Git::new();
    let found = find_registry(&dotted, None, None, &git).unwrap();
    assert_eq!(found.root, meta);
    place(&ws, Some(&real));
    let found = find_registry(&dotted, None, None, &git).unwrap();
    assert_eq!(found.root, ws);
    assert_eq!(found.path, ws.join(REGISTRY_FILE));
    // a start that doesn't exist finds nothing
    let e = find_registry(&elsewhere.join("decoy/../nope"), None, None, &git).unwrap_err();
    assert!(matches!(e, Error::RegistryNotFound { .. }), "{e}");
}

#[test]
fn git_is_asked_only_where_a_linked_worktree_may_hold_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    place(&repo, None);
    let deep = repo.join("src");
    std::fs::create_dir(&deep).unwrap();

    // no `.git` above, or a dir as a main checkout's is: no spawn
    let git = Git::new();
    assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
    checkout(&repo);
    assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
    assert_eq!(git.spawns(), 0);

    // a `.git` file, as a linked worktree's is: git decides, and a
    // failure keeps the registry where it's found
    std::fs::remove_dir(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git"), "gitdir: nowhere\n").unwrap();
    assert_eq!(find_registry(&deep, None, None, &git).unwrap().root, repo);
    assert_eq!(git.spawns(), 1);
}

#[test]
fn a_discovered_root_is_checked_only_inside_a_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    place(&ws, None);
    let es = entries(&["meta"]);
    let git = Git::new();
    // outside any checkout: no spawn
    let found = find_registry(&ws, None, None, &git).unwrap();
    check_discovered_root(&found, &es, &git).unwrap();
    assert_eq!(git.spawns(), 0);
    // named by `--root` or `--registry`: never checked
    checkout(&ws);
    let root = find_registry(&ws, None, Some(&ws), &git).unwrap();
    let explicit = find_registry(&ws, Some(Path::new("repos.toml")), None, &git).unwrap();
    for loc in [&root, &explicit] {
        assert!(!loc.discovered);
        check_discovered_root(loc, &es, &git).unwrap();
    }
    assert_eq!(git.spawns(), 0);
    // discovered in one: git reads its origin, as configured and as
    // resolved — here there's none
    let found = find_registry(&ws, None, None, &git).unwrap();
    check_discovered_root(&found, &es, &git).unwrap();
    assert_eq!(git.spawns(), 2);
}

#[test]
fn an_explicit_registry_roots_at_its_dir_links_or_not() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let real = place(&ws.join("meta"), None);
    place(&ws, Some(&real));
    let found = find_registry(&ws, Some(Path::new("meta/repos.toml")), None, &Git::new()).unwrap();
    assert_eq!(found.root, ws.join("meta"));
    assert_eq!(found.path, real);
}

#[test]
fn explicit_path_is_relative_to_cwd() {
    let found = find_registry(
        Path::new("/a/b"),
        Some(Path::new("../reg/repos.toml")),
        None,
        &Git::new(),
    )
    .unwrap();
    assert_eq!(found.path, Path::new("/a/b/../reg/repos.toml"));
    assert_eq!(found.root, Path::new("/a/b/../reg"));
}

#[test]
fn root_overrides_the_registry_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let meta = tmp.path().join("meta");
    std::fs::create_dir_all(ws.join("repo")).unwrap();
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join(REGISTRY_FILE), "owners = []\n").unwrap();

    // with an explicit registry
    let found = find_registry(
        &ws,
        Some(&meta.join(REGISTRY_FILE)),
        Some(Path::new(".")),
        &Git::new(),
    )
    .unwrap();
    assert_eq!(found.path, meta.join(REGISTRY_FILE));
    assert_eq!(found.root, ws.join("."));

    // with the walk-up
    let found = find_registry(&meta, None, Some(&ws), &Git::new()).unwrap();
    assert_eq!(found.path, meta.join(REGISTRY_FILE));
    assert_eq!(found.root, ws);
}

#[test]
fn a_missing_root_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(REGISTRY_FILE), "owners = []\n").unwrap();
    let e = find_registry(tmp.path(), None, Some(Path::new("nope")), &Git::new()).unwrap_err();
    assert!(matches!(e, Error::RootNotFound { .. }), "{e}");
    assert_eq!(e.exit_code(), 2);
}

#[test]
fn not_found_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let git = Git::new();
    let e = find_registry(tmp.path(), None, None, &git).unwrap_err();
    assert!(matches!(e, Error::RegistryNotFound { .. }));
    assert_eq!(git.spawns(), 1);
    assert!(e.hint().is_some_and(|h| h.contains("--registry")), "{e}");
}

#[test]
fn edit_distance_counts_each_edit_once() {
    for (a, b, d) in [
        ("", "", 0),
        ("gro", "gro", 0),
        ("gro", "", 3),
        ("gro", "gor", 1),  // transposition
        ("gro", "grp", 1),  // substitution
        ("gro", "groo", 1), // insertion
        ("fuz_ui", "fuz_css", 3),
        ("kitten", "sitting", 3),
        ("ca", "abc", 3),  // optimal string alignment, not full Damerau
        ("zzz", "żzz", 1), // chars, not bytes
    ] {
        assert_eq!(edit_distance(a, b), d, "{a} / {b}");
        assert_eq!(edit_distance(b, a), d, "{b} / {a}");
    }
}

/// Entries keyed `keys`, each in the dir of its name; `key:dir` names
/// another dir.
fn entries(keys: &[&str]) -> Vec<Entry> {
    use std::fmt::Write as _;
    let mut toml = String::from("owners = [\"me\"]\n");
    for k in keys {
        let (k, dir) = k.split_once(':').unwrap_or((k, k));
        let _ = write!(
            toml,
            "[repos.{k}]\nurl = \"https://github.com/me/{k}\"\nvisibility = \"public\"\n\
             purpose = \"x\"\ndir = \"{dir}\"\n"
        );
    }
    crate::registry::Registry::parse(&toml)
        .unwrap()
        .validate()
        .unwrap()
        .entries()
}

#[test]
fn suggests_close_keys_best_first() {
    let es = entries(&[
        "fuz_app", "fuz_css", "fuz_ui", "fuz_util", "gro", "graphite", "tsv", "zzz", "FuzDocs",
        "cm",
    ]);
    let suggest = |t: &str| suggest_keys(&es, t);
    // a typo, a transposition, a case slip
    assert_eq!(suggest("gor"), ["gro"]);
    assert_eq!(suggest("fzu_ui"), ["fuz_ui"]);
    assert_eq!(suggest("GRO"), ["gro"]);
    // the key's case ignored too, and suggested as declared
    assert_eq!(suggest("fuzdoc"), ["FuzDocs"]);
    // near ones by distance, then key; capped
    assert_eq!(suggest("fuz_u"), ["fuz_ui", "fuz_util"]);
    assert_eq!(suggest("fuz"), ["fuz_ui", "FuzDocs", "fuz_app"]);
    // a prefix too far to be a typo, and a key inside the target
    assert_eq!(suggest("grap"), ["graphite"]);
    assert_eq!(suggest("gro-old"), ["gro"]);
    // a path's last component
    assert_eq!(suggest("../tsb/"), ["tsv"]);
    assert_eq!(suggest("/elsewhere/zzz"), ["zzz"]);
    // a 2-char name still allows one edit
    assert_eq!(suggest("cn"), ["cm"]);
    // containment needs three chars: `fu` is in every `fuz_*`, and too
    // far from each by edits
    assert!(suggest("fu").is_empty());
    // nothing close
    assert!(suggest("mageguild").is_empty());
    assert!(suggest(".").is_empty());
    assert!(suggest("..").is_empty());
    assert!(suggest("").is_empty());
    // short names match short keys only by edits, never by containment
    assert!(suggest("z").is_empty());
    assert_eq!(suggest("zz"), ["zzz"]);
}

#[test]
fn suggests_by_key_or_dir_named_by_key() {
    let es = entries(&["app_forge:vendor_app_forge", "app_os:vendor_app_os", "tsv"]);
    let suggest = |t: &str| suggest_keys(&es, t);
    // near the dir by one edit, and `app_os`'s dir by three
    assert_eq!(suggest("vendor_app_forg"), ["app_forge", "app_os"]);
    // the closer of key and dir ranks it: `app_os` by its key
    assert_eq!(suggest("app_o"), ["app_os"]);
    assert_eq!(suggest("../vendor_app_os/"), ["app_os", "app_forge"]);
}
