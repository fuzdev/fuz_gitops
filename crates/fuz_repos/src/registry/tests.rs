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
url = "https://github.com/me/hidden_site.git"
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

[references.wpt]
url = "https://github.com/me/wpt"
upstream = "https://github.com/them/wpt"
purpose = "pinned, its commit on a branch"
branch = "fork"
pinned = true
"#;

#[test]
fn parses_core_and_ignores_grimoire() {
    let r = Registry::parse(MINIMAL).unwrap();
    assert_eq!(r.owners, ["me"]);
    assert_eq!(r.repos.len(), 2);
    assert_eq!(r.references.len(), 4);
    let site = &r.repos["site"];
    assert_eq!(site.url.name, "hidden_site");
    assert_eq!(site.dir.as_deref(), Some("site"));
    assert_eq!(site.requires, ["spec"]);
}

#[test]
fn branch_and_pinned_are_independent() {
    let r = Registry::parse(MINIMAL).unwrap();
    let checkout = |key: &str| {
        let e = &r.references[key];
        (e.branch.as_deref(), e.pinned)
    };
    assert_eq!(checkout("spec"), (Some("fork"), false));
    assert_eq!(checkout("oracle"), (None, true));
    assert_eq!(checkout("loose"), (None, false));
    assert_eq!(checkout("wpt"), (Some("fork"), true));
}

#[test]
fn resolves_entries() {
    let r = Registry::parse(MINIMAL).unwrap().validate().unwrap();
    let entries = r.entries();
    let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, ["app", "site", "loose", "oracle", "spec", "wpt"]);

    let app = &entries[0];
    assert_eq!(app.dir, "app");
    assert!(app.writable && app.ci && !app.archived);
    assert_eq!((app.branch.as_deref(), app.pinned), (Some("main"), false));

    let site = &entries[1];
    assert_eq!(site.dir, "site");
    assert!(site.ci && site.archived);
    assert_eq!(
        (site.branch.as_deref(), site.pinned),
        (Some("trunk"), false)
    );

    let loose = &entries[2];
    assert_eq!(loose.dir, "loose");
    assert!(!loose.writable && !loose.ci && loose.visibility.is_none());
    assert_eq!((loose.branch.as_deref(), loose.pinned), (None, false));

    let oracle = &entries[3];
    assert_eq!((oracle.branch.as_deref(), oracle.pinned), (None, true));

    let spec = &entries[4];
    assert!(spec.writable);
    assert_eq!((spec.branch.as_deref(), spec.pinned), (Some("fork"), false));

    let wpt = &entries[5];
    assert!(wpt.writable);
    assert_eq!((wpt.branch.as_deref(), wpt.pinned), (Some("fork"), true));
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
fn pinned_must_be_a_bool() {
    let e = Registry::parse(
        r#"
owners = []
[references.x]
url = "https://github.com/them/x"
purpose = "x"
pinned = "yes"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("line 6"), "{e}");
}

#[test]
fn an_empty_branch_is_an_error_with_position() {
    for (table, extra) in [("repos", "visibility = \"public\"\n"), ("references", "")] {
        let src = format!(
            "owners = [\"me\"]\n[{table}.x]\nurl = \"https://github.com/me/x\"\n\
             purpose = \"x\"\n{extra}branch = \"\"\n"
        );
        let e = Registry::parse(&src).unwrap_err().to_string();
        assert!(e.contains("branch is empty"), "{table}: {e}");
        let line = if extra.is_empty() { 5 } else { 6 };
        assert!(e.contains(&format!("line {line}")), "{table}: {e}");
        // a named branch parses, and none is the default
        let named = src.replace("branch = \"\"", "branch = \"trunk\"");
        assert!(Registry::parse(&named).is_ok(), "{table}");
        let absent = src.replace("branch = \"\"\n", "");
        assert!(Registry::parse(&absent).is_ok(), "{table}");
    }
}

#[test]
fn pinned_is_references_only() {
    let e = Registry::parse(
        r#"
owners = ["me"]
[repos.x]
url = "https://github.com/me/x"
visibility = "public"
purpose = "x"
pinned = true
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("pinned"), "{e}");
    assert!(e.contains("line 7"), "{e}");
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
        "hidden_site",
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
        let src = format!("owners = [\"me\"]\n[references.r]\nurl = \"{url}\"\npurpose = \"x\"\n");
        let e = Registry::parse(&src).unwrap_err().to_string();
        assert!(e.contains("the account and name must be"), "{e}");
    }
}

#[test]
fn sparse_is_a_relative_path_of_plain_names_checked_at_parse() {
    let parse = |sparse: &str| {
        let src = format!(
            "owners = [\"me\"]\n[references.r]\nurl = \"https://github.com/them/r\"\n\
             purpose = \"x\"\nsparse = {}\n",
            // a JSON string is a valid TOML basic string
            serde_json::to_string(sparse).unwrap()
        );
        Registry::parse(&src).map(|r| r.references["r"].sparse.clone())
    };
    for bad in [
        "",
        ".",
        "..",
        "/css",
        "css/",
        "a//b",
        "./css",
        "css/.",
        "../css",
        "a/../b",
        "*",
        "css/*.css",
        "c?s",
        "[a]",
        "a\\b",
        "a\tb",
        "a\nb",
        "a\u{1b}b",
    ] {
        let e = parse(bad).unwrap_err();
        let message = e.to_string();
        assert!(
            message.contains("isn't a relative path of plain directory names"),
            "{bad:?}: {message}"
        );
        // positioned at the value
        assert!(e.span().is_some(), "{bad:?}");
    }
    for good in [
        "css",
        "css/deep",
        "a.b",
        "..x",
        "x..",
        ".hidden/y",
        "sp ace",
        "é",
    ] {
        assert_eq!(parse(good).unwrap(), Some(good.to_owned()), "{good:?}");
    }
    // absent is none
    let src = "owners = [\"me\"]\n[references.r]\nurl = \"https://github.com/them/r\"\n\
               purpose = \"x\"\n";
    assert_eq!(Registry::parse(src).unwrap().references["r"].sparse, None);
}

#[test]
fn entries_naming_one_repo_name_each_other() {
    let src = "owners = [\"me\"]\n\
        [repos.app]\nurl = \"https://github.com/me/app\"\nvisibility = \"public\"\n\
        purpose = \"x\"\n\
        [repos.app_wt]\nurl = \"https://GitHub.com/Me/App.git\"\nvisibility = \"public\"\n\
        purpose = \"x\"\n\
        [repos.other]\nurl = \"https://github.com/me/other\"\nvisibility = \"public\"\n\
        purpose = \"x\"\n\
        [references.app_ref]\nurl = \"https://github.com/me/app\"\npurpose = \"x\"\n\
        dir = \"app-ref\"\n";
    let entries = Registry::parse(src).unwrap().validate().unwrap().entries();
    let same: Vec<(&str, Option<&str>)> = entries
        .iter()
        .map(|e| (e.key.as_str(), e.same_repo_as.as_deref()))
        .collect();
    assert_eq!(
        same,
        [
            ("app", Some("app_wt")),
            ("app_wt", Some("app")),
            ("other", None),
            ("app_ref", Some("app")),
        ]
    );
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
url = "https://github.com/me/hidden_site"
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
