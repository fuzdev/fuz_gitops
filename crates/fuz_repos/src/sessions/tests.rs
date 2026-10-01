use std::path::Component;

use super::*;

#[test]
fn claudecode_set_is_an_agent() {
    let caller = |v: Option<&str>| Caller::from_claudecode(v.map(std::ffi::OsStr::new));
    assert_eq!(caller(Some("1")), Caller::Agent);
    assert_eq!(caller(Some("0")), Caller::Agent);
    assert_eq!(caller(Some("")), Caller::Person);
    assert_eq!(caller(None), Caller::Person);
}

#[test]
fn starttime_is_field_22_after_the_last_paren() {
    let tail = "S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 50111550 5980160 434";
    assert_eq!(
        stat_starttime(&format!("3291533 (cat) {tail}")),
        Some(50_111_550)
    );
    // a name with spaces and parens of its own
    assert_eq!(
        stat_starttime(&format!("7 (a ) b) (c)) {tail}")),
        Some(50_111_550)
    );
    assert_eq!(
        stat_starttime(&format!("7 (x) y) {tail}")),
        Some(50_111_550)
    );
    // too few fields, no name, a field that isn't a number
    assert_eq!(stat_starttime("7 (cat) S 1 2 3"), None);
    assert_eq!(stat_starttime(tail), None);
    assert_eq!(
        stat_starttime("7 (cat) S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 x 5"),
        None
    );
}

#[test]
fn ppid_is_field_4_after_the_last_paren() {
    let tail = "S 1 2 3 0 -1 4194304 97 0 0 0 0 0 0 0 20 0 1 0 50111550 5980160 434";
    assert_eq!(stat_ppid(&format!("3291533 (cat) {tail}")), Some(1));
    assert_eq!(stat_ppid(&format!("7 (a ) 9 (c)) {tail}")), Some(1));
    assert_eq!(stat_ppid("7 (cat) S"), None);
    assert_eq!(stat_ppid("7 (cat) S x"), None);
}

#[test]
fn the_ancestors_are_the_ppid_chain() {
    let ancestors = caller_ancestors();
    let parent = std::os::unix::process::parent_id();
    let stat = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    assert_eq!(
        ancestors.get(&parent).copied(),
        stat_starttime(&stat(parent)),
        "{ancestors:?}"
    );
    assert!(!ancestors.contains_key(&std::process::id()));
    // each one's parent is the next, up to pid 1 — itself included when
    // the chain reaches it (not when a parent is outside the namespace)
    let mut pid = parent;
    while pid > 1 {
        assert!(ancestors.contains_key(&pid), "{pid}: {ancestors:?}");
        pid = stat_ppid(&stat(pid)).unwrap();
    }
    if pid == 1 {
        assert_eq!(
            ancestors.get(&1).copied(),
            stat_starttime(&stat(1)),
            "{ancestors:?}"
        );
    } else {
        assert!(!ancestors.contains_key(&1), "{ancestors:?}");
    }
}

/// Walks up from a process whose ppid is 50 and started at 500, over
/// `chain` (pid to ppid and start time); a pid it lacks can't be read.
fn walk(chain: &[(u32, (u32, u64))]) -> BTreeMap<u32, u64> {
    let chain: BTreeMap<u32, (u32, u64)> = chain.iter().copied().collect();
    walk_ancestors((50, 500), |pid| {
        assert!(pid > 0, "looked up pid {pid}");
        chain.get(&pid).copied()
    })
}

#[test]
fn the_ancestor_walk_follows_ppids_while_they_hold() {
    // up to and including pid 1, never past it, whatever its ppid reads
    let to_init = [
        (50, (40, 400)),
        (40, (30, 300)),
        (30, (1, 1)),
        (1, (7, 0)),
        (7, (0, 0)),
    ];
    assert_eq!(
        walk(&to_init),
        BTreeMap::from([(50, 400), (40, 300), (30, 1), (1, 0)])
    );
    // a Claude Code running as pid 1, this process's parent
    assert_eq!(
        walk_ancestors((1, 500), |pid| {
            assert_eq!(pid, 1, "looked up pid {pid}");
            Some((7, 0))
        }),
        BTreeMap::from([(1, 0)])
    );
    // not pid 0, a parent outside the pid namespace
    let to_zero = [(50, (40, 400)), (40, (0, 300))];
    assert_eq!(walk(&to_zero), BTreeMap::from([(50, 400), (40, 300)]));
    assert!(walkfrom((0, 500)).is_empty());
    // a parent started after its child: its pid was reused, so neither
    // it nor anything above it is an ancestor
    let reused = [(50, (40, 400)), (40, (30, 401)), (30, (20, 1))];
    assert_eq!(walk(&reused), BTreeMap::from([(50, 400)]));
    let reused_first = [(50, (40, 501))];
    assert!(walk(&reused_first).is_empty());
    // started in the same tick as its child: still its parent
    let same_tick = [(50, (40, 500)), (40, (1, 500))];
    assert_eq!(walk(&same_tick), BTreeMap::from([(50, 500), (40, 500)]));
    // a pid that can't be read stops the walk
    let unreadable = [(50, (40, 400)), (30, (1, 1))];
    assert_eq!(walk(&unreadable), BTreeMap::from([(50, 400)]));
    // a chain that never ends is cut at the cap
    let endless = walk_ancestors((1000, 0), |pid| Some((pid + 1, 0)));
    assert_eq!(endless.len(), MAX_ANCESTRY);
    assert_eq!(
        endless.keys().max(),
        Some(&(1000 + u32::try_from(MAX_ANCESTRY).unwrap() - 1))
    );
}

/// Walks up from `own` over a chain with nothing in it.
fn walkfrom(own: (u32, u64)) -> BTreeMap<u32, u64> {
    walk_ancestors(own, |pid| panic!("looked up pid {pid}"))
}

/// A reader whose pause counts itself instead of sleeping.
fn counting_reader() -> (Reader, std::rc::Rc<std::cell::Cell<usize>>) {
    let pauses = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = std::rc::Rc::clone(&pauses);
    let reader = Reader {
        pause: Box::new(move || counter.set(counter.get() + 1)),
        ..Reader::default()
    };
    (reader, pauses)
}

#[test]
fn a_torn_read_is_retried_once() {
    let unreadable = || Unavailable::Unreadable {
        path: "/x".into(),
        error: "e".into(),
    };
    let unparseable = || Unavailable::Unparseable {
        path: "/x".into(),
        error: "e".into(),
    };
    // `results(n)` is the nth call's: the calls made, the pauses, and
    // what came of it
    let run = |results: &dyn Fn(usize) -> Step<u8>| {
        let (mut reader, pauses) = counting_reader();
        let mut calls = 0;
        let got = reader.retry_torn(|_| {
            calls += 1;
            results(calls)
        });
        (calls, pauses.get(), got)
    };
    assert_eq!(run(&|_| Ok(7)), (1, 0, Ok(7)));
    // torn, then whole
    for torn in [unreadable(), unparseable()] {
        let whole = |n| if n == 1 { Err(torn.clone()) } else { Ok(7) };
        assert_eq!(run(&whole), (2, 1, Ok(7)));
        // torn for good: read twice, and it counts
        assert_eq!(run(&|_| Err(torn.clone())), (2, 1, Err(torn.clone())));
    }
    // nothing to wait out
    for reason in [
        Unavailable::HomeUnknown,
        Unavailable::RelativeConfigDir { path: "x".into() },
        Unavailable::ForeignPidDomain {
            path: "/x".into(),
            pid_domain: "d".into(),
            source: SessionSource::SessionFile,
        },
    ] {
        assert_eq!(run(&|_| Err(reason.clone())), (1, 0, Err(reason.clone())));
    }
}

#[test]
fn a_file_rewritten_during_the_pause_is_read_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("claude");
    let pid = std::process::id();
    let start = read_stat(Path::new("/proc/self/stat"))
        .unwrap()
        .1
        .to_string();
    let domain = Reader::default().own_domain().unwrap().to_owned();
    let source = SessionsSource {
        config_dirs: Ok(vec![dir.clone()]),
        claude_pid: None,
        ancestors: BTreeMap::new(),
    };
    let session_file = dir.join(format!("sessions/{pid}.json"));
    let roster = dir.join("daemon/roster.json");
    let here = std::env::current_dir()
        .unwrap()
        .canonicalize()
        .unwrap()
        .into_os_string()
        .into_string()
        .unwrap();
    assert_ne!(here, "/");
    let docs = [
        (
            &session_file,
            serde_json::json!({
                "pid": pid, "procStart": start, "cwd": "/", "pidDomain": domain,
            }),
            SessionSource::SessionFile,
        ),
        (
            &roster,
            serde_json::json!({"workers": {"w": {"pid": pid, "procStart": start, "cwd": "/"}}}),
            SessionSource::RosterWorker,
        ),
    ];
    for (file, doc, recorded_as) in docs {
        let whole = doc.to_string();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, &whole[..whole.len() / 2]).unwrap();
        // half-written, then whole once the reader pauses
        let (mut reader, pauses) = counting_reader();
        let (path, rewrite) = (file.clone(), whole.clone());
        let mut count = reader.pause;
        reader.pause = Box::new(move || {
            count();
            std::fs::write(&path, &rewrite).unwrap();
        });
        assert_eq!(reader.read(&source), Ok(()));
        assert_eq!(pauses.get(), 1);
        // this process isn't at `/`: where it is joins the session
        assert_eq!(
            reader.live.into_values().collect::<Vec<_>>(),
            [Session {
                process_cwd: Some(here.clone()),
                ..Session::at(pid, start.parse().unwrap(), "/".into(), recorded_as)
            }]
        );
        // left half-written: read twice, and it can't be vouched for
        std::fs::write(file, &whole[..whole.len() / 2]).unwrap();
        let (mut reader, pauses) = counting_reader();
        assert!(
            matches!(reader.read(&source), Err(Unavailable::Unparseable { path, .. })
                if path == file.to_string_lossy()),
        );
        assert_eq!(pauses.get(), 1);
        std::fs::remove_file(file).unwrap();
    }
}

#[test]
fn the_caller_is_an_ancestor_or_nobody() {
    let source = |claude_pid| SessionsSource {
        config_dirs: Ok(vec![]),
        claude_pid,
        ancestors: BTreeMap::from([(10, 100), (20, 50)]),
    };
    assert_eq!(source(Some(10)).caller(), Some((10, 100)));
    assert_eq!(source(Some(11)).caller(), None);
    assert_eq!(source(None).caller(), None);
    let worker = |repl_pid, repl_proc_start: Option<&str>| WorkerRecord {
        proc_start: "1".into(),
        cwd: "/".into(),
        pid_domain: None,
        repl_pid,
        repl_proc_start: repl_proc_start.map(Into::into),
        worktree_path: None,
    };
    assert!(worker(Some(10), Some("100")).is_callers((10, 100)));
    assert!(worker(Some(10), None).is_callers((10, 100)));
    // the pid reused, or unreadable: not the caller
    assert!(!worker(Some(10), Some("101")).is_callers((10, 100)));
    assert!(!worker(Some(10), Some("x")).is_callers((10, 100)));
    assert!(!worker(Some(11), Some("100")).is_callers((10, 100)));
    assert!(!worker(None, None).is_callers((10, 100)));
}

#[test]
fn config_dirs_are_both_once_each() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let other = base.join("other");
    let dirs = |config: Option<&Path>, home: Option<&Path>| {
        config_dirs(
            config.map(|p| p.as_os_str().to_owned()),
            home.map(|p| p.as_os_str().to_owned()),
        )
    };
    assert_eq!(
        dirs(Some(&other), Some(&home)),
        Ok(vec![other.clone(), home.join(".claude")])
    );
    assert_eq!(dirs(None, Some(&home)), Ok(vec![home.join(".claude")]));
    // without a home, `~/.claude` can't be found: a `CLAUDE_CONFIG_DIR`
    // alone isn't every dir
    assert_eq!(dirs(Some(&other), None), Err(Unavailable::HomeUnknown));
    assert_eq!(dirs(None, None), Err(Unavailable::HomeUnknown));
    // the same dir, however spelled, is read once
    let link = base.join("link");
    std::os::unix::fs::symlink(home.join(".claude"), &link).unwrap();
    assert_eq!(dirs(Some(&link), Some(&home)), Ok(vec![link.clone()]));
    assert_eq!(
        dirs(Some(&home.join(".claude/.")), Some(&home)).map(|d| d.len()),
        Ok(1)
    );
    // a relative one is kept as given, for the reader to refuse
    let relative = Path::new("rel");
    assert_eq!(
        dirs(Some(relative), Some(&home)),
        Ok(vec![relative.to_owned(), home.join(".claude")])
    );
    assert_eq!(
        dirs(Some(&link), Some(relative)),
        Ok(vec![link, relative.join(".claude")])
    );
}

#[test]
fn a_relative_config_dir_naming_a_read_one_is_refused_not_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    // `..` up to `/` from the tool's cwd, then the absolute path: from
    // here it names the very dir `home` does
    let cwd = std::env::current_dir().unwrap();
    let up: PathBuf = cwd
        .components()
        .skip(1)
        .map(|_| Component::ParentDir)
        .collect();
    let relative = up.join(claude.strip_prefix("/").unwrap());
    assert!(relative.is_relative());
    assert_eq!(relative.canonicalize().unwrap(), claude);
    let relative_home = up.join(home.strip_prefix("/").unwrap());
    for (config, home) in [(relative, home), (claude, relative_home)] {
        let dirs = config_dirs(Some(config.into_os_string()), Some(home.into_os_string()));
        assert_eq!(dirs.as_ref().map(Vec::len), Ok(2), "{dirs:?}");
        let source = SessionsSource {
            config_dirs: dirs,
            claude_pid: None,
            ancestors: BTreeMap::new(),
        };
        assert!(
            matches!(
                read_live_sessions(&source),
                LiveSessions::Unavailable(Unavailable::RelativeConfigDir { path })
                    if Path::new(&path).is_relative()
            ),
            "{source:?}"
        );
    }
}

#[test]
fn session_files_are_pid_dot_json() {
    assert_eq!(session_file_pid("123.json"), Some(123));
    for name in [
        "123.abc.key",
        "123.json.tmp",
        "+123.json",
        "-1.json",
        ".json",
        "abc.json",
        "123",
        "99999999999.json",
    ] {
        assert_eq!(session_file_pid(name), None, "{name}");
    }
}

#[test]
fn records_parse_leniently_but_need_their_fields() {
    let full = r#"{"pid":7,"procStart":"42","cwd":"/ws/app","pidDomain":"d",
            "status":"idle","somethingNew":{"x":1}}"#;
    let r: SessionRecord = serde_json::from_str(full).unwrap();
    assert_eq!(
        (r.pid, r.proc_start.as_str(), r.cwd.as_str()),
        (7, "42", "/ws/app")
    );
    let no_domain = r#"{"pid":7,"procStart":"42","cwd":"/ws/app"}"#;
    assert!(serde_json::from_str::<SessionRecord>(no_domain).is_err());
    // a worker records no domain
    let w: WorkerRecord = serde_json::from_str(no_domain).unwrap();
    assert_eq!(
        (w.pid_domain, w.repl_pid, w.repl_proc_start, w.worktree_path),
        (None, None, None, None)
    );
    assert!(parse_proc_start("42").is_ok());
    for bad in ["", "-1", "4 2", "0x2a"] {
        assert!(parse_proc_start(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_lock_reason_is_read_as_claude_code_reads_it() {
    let lock = |pid, start| Some(ClaudeLock { pid, start });
    for (reason, want) in [
        (
            "claude agent agent-a1 (pid 42 start 123)",
            lock(42, Some("123")),
        ),
        ("claude agent agent-a1 (pid 42)", lock(42, None)),
        (
            "claude session feat/x (pid 42 start 123)",
            lock(42, Some("123")),
        ),
        // the name takes anything but a line break, and is greedy
        ("claude agent a b) (c (pid 42)", lock(42, None)),
        (
            "claude agent a (pid 1) b (pid 42 start 7)",
            lock(42, Some("7")),
        ),
        ("claude agent a (pid 1 start 2) (pid 3)", lock(3, None)),
        // the last ` (pid ` leaves `9))`, so an earlier one wins
        (
            "claude agent x (pid 5 start 7 (pid 9))",
            lock(5, Some("7 (pid 9)")),
        ),
        // `\d{1,10}`, taken as a number
        ("claude agent a (pid 0042)", lock(42, None)),
        ("claude agent a (pid 9999999999)", lock(9_999_999_999, None)),
        ("claude agent a (pid 42 start x y)", lock(42, Some("x y"))),
    ] {
        assert_eq!(claude_lock(reason), want, "{reason:?}");
    }
    for reason in [
        "",
        "claude agent a (pid )",
        "claude agent a (pid 12345678901)",
        "claude agent  (pid 42)",
        "claude agent a (pid 42 start )",
        "claude agent a (pid 42 start 7",
        "claude agent a (pid 42) ",
        "claude agent a (pid 42)\n",
        "claude agent a\nb (pid 42)",
        "claude agent a\rb (pid 42)",
        "claude agent a (pid 42 start 7\u{2028})",
        "claude agent a (pid 42 start 7\u{2029})",
        "claude agent a (pid -42)",
        "claude agent a (pid 42,start 7)",
        "claude worker a (pid 42)",
        "claude  agent a (pid 42)",
        "agent a (pid 42)",
    ] {
        assert_eq!(claude_lock(reason), None, "{reason:?}");
    }
    // 1 to 255 UTF-16 code units each
    let name = "n".repeat(255);
    assert!(claude_lock(&format!("claude agent {name} (pid 42)")).is_some());
    assert!(claude_lock(&format!("claude agent {name}n (pid 42)")).is_none());
    let astral = "\u{1F600}".repeat(127);
    assert!(claude_lock(&format!("claude agent {astral} (pid 42 start {astral})")).is_some());
    let astral = "\u{1F600}".repeat(128);
    assert!(claude_lock(&format!("claude agent {astral} (pid 42)")).is_none());
    assert!(claude_lock(&format!("claude agent a (pid 42 start {astral})")).is_none());
    // 3 UTF-8 bytes to the unit, the most bytes 255 units can take
    let wide = "\u{20AC}".repeat(255);
    assert!(claude_lock(&format!("claude agent {wide} (pid 42 start {wide})")).is_some());
    assert!(claude_lock(&format!("claude agent {wide}\u{20AC} (pid 42)")).is_none());
}

#[test]
fn a_huge_lock_reason_is_rejected_in_linear_time() {
    // every ` (pid ` is a candidate split, each over an ever longer name
    let reason = format!("claude agent a{}", " (pid 1".repeat(320 * 1024 / 7));
    let started = std::time::Instant::now();
    assert_eq!(claude_lock(&reason), None);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_lock_names_a_session_by_pid_and_start_time_to_the_digit() {
    let s = Session::at(
        42,
        123,
        "/nonexistent-ws".into(),
        SessionSource::SessionFile,
    );
    let names = |reason: &str| claude_lock(reason).unwrap().names(&s);
    assert!(names("claude agent a (pid 42 start 123)"));
    assert!(names("claude agent a (pid 42)"));
    assert!(names("claude agent a (pid 0042)"));
    assert!(!names("claude agent a (pid 43 start 123)"));
    assert!(!names("claude agent a (pid 43)"));
    assert!(!names("claude agent a (pid 42 start 124)"));
    assert!(!names("claude agent a (pid 42 start 0123)"));
    assert!(!names("claude agent a (pid 42 start 123 )"));
    assert!(!names("claude agent a (pid 4294967338)"));
    // the start compares as `to_string` spells it, without making one
    for n in [0, 7, 10, 123, 1_000_000, u64::MAX] {
        assert!(is_decimal_of(&n.to_string(), n), "{n}");
        assert!(!is_decimal_of(&format!("0{n}"), n), "{n}");
        assert!(!is_decimal_of(&format!("+{n}"), n), "{n}");
        assert!(!is_decimal_of(&format!("{n}0"), n), "{n}");
        assert!(!is_decimal_of(&format!("1{n}"), n), "{n}");
    }
    assert!(!is_decimal_of("", 0));
    assert!(!is_decimal_of("18446744073709551616", u64::MAX));
}
