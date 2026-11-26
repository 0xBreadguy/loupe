use super::*;
use crate::storage_view::data::{Entry, Project, Sizes};
use rusqlite::{Connection, params};
use serde_json::json;

const GB: u64 = 1_000_000_000;
const NOW: f64 = 1_795_026_000_000.;

fn ok(json: &str) -> CleanupSettings {
    match parse(json) {
        RulesRead::Ok(settings) => settings,
        other => panic!("expected settings, got {other:?}"),
    }
}

fn invalid(json: &str) -> (&'static str, &'static str) {
    match parse(json) {
        RulesRead::Invalid { key, constraint } => (key, constraint),
        other => panic!("expected invalid, got {other:?}"),
    }
}

#[test]
fn defaults_remove_nothing_and_auto_settle_after_seven_days() {
    let settings = ok("{}");
    assert_eq!(settings, CleanupSettings::default());
    assert!(!resolve(&settings, None).enabled());
    assert_eq!(settings.auto_settle_days, Some(7.));
    assert!(settings.auto_settle_on_merge);
    assert_eq!(
        RulesRead::Absent.settings().unwrap().into_owned(),
        CleanupSettings::default()
    );
}

#[test]
fn resolver_follows_t3_for_every_policy_combination() {
    let storage =
        r#""storageCleanup":{"worktreeKeepWhen":"tracked-changes","worktreeAfterDays":30}"#;
    // Top-level null and absent both use storageCleanup.
    for top in ["", r#""worktreeCleanup":null,"#] {
        let s = ok(&format!("{{{top}{storage}}}"));
        let rules = resolve(&s, Some("p"));
        assert_eq!(rules.after_days, Some(30));
        assert_eq!(rules.keep_when, KeepWhen::TrackedChanges);
    }
    // Top-level off keeps storageCleanup's keepWhen and turns every trigger off.
    let s = ok(&format!(
        r#"{{"worktreeCleanup":{{"mode":"off"}},{storage}}}"#
    ));
    let rules = resolve(&s, None);
    assert!(!rules.enabled());
    assert_eq!(rules.keep_when, KeepWhen::TrackedChanges);
    // Top-level custom wins over storageCleanup.
    let s = ok(&format!(
        r#"{{"worktreeCleanup":{{"mode":"custom","rules":{{"worktreeAfterDays":null,"worktreeSettledAfterDays":3,"worktreeOnMerge":true,"worktreeOnDelete":false,"worktreeUnchanged":false}}}},{storage}}}"#
    ));
    let rules = resolve(&s, Some("other"));
    assert_eq!(rules.settled_after_days, Some(3));
    assert_eq!(rules.after_days, None);
    assert!(rules.on_merge);
    assert_eq!(
        rules.keep_when,
        KeepWhen::UncommittedChanges,
        "custom rules default keepWhen"
    );
    // An explicit null override means storageCleanup even when the top level is custom.
    let s = ok(&format!(
        r#"{{"worktreeCleanup":{{"mode":"off"}},{storage},"projectSettingsOverrides":{{"a":{{"worktreeCleanup":null}},"b":{{"defaultRuntimeMode":"full-access"}},"c":{{"worktreeCleanup":{{"mode":"off"}}}}}}}}"#
    ));
    assert_eq!(resolve(&s, Some("a")).after_days, Some(30));
    assert!(
        !resolve(&s, Some("b")).enabled(),
        "an absent key inherits the top level"
    );
    assert!(!resolve(&s, Some("c")).enabled());
    assert!(s.overrides.contains_key("a") && s.overrides.contains_key("c"));
    assert!(
        !s.overrides.contains_key("b"),
        "other override keys are not kept"
    );
}

#[test]
fn values_outside_t3s_schema_are_named_by_key_and_never_clamped() {
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeAfterDays":0}}"#),
        ("storageCleanup.worktreeAfterDays", DAYS_1)
    );
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeAfterDays":3651}}"#),
        ("storageCleanup.worktreeAfterDays", DAYS_1)
    );
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeAfterDays":1.5}}"#),
        ("storageCleanup.worktreeAfterDays", DAYS_1)
    );
    assert_eq!(
        ok(r#"{"storageCleanup":{"worktreeSettledAfterDays":0}}"#)
            .storage
            .settled_after_days,
        Some(0)
    );
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeSettledAfterDays":-1}}"#),
        ("storageCleanup.worktreeSettledAfterDays", DAYS_0)
    );
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeKeepWhen":"always"}}"#).0,
        "storageCleanup.worktreeKeepWhen"
    );
    assert_eq!(
        invalid(r#"{"storageCleanup":{"worktreeOnMerge":"yes"}}"#),
        ("storageCleanup.worktreeOnMerge", BOOL)
    );
    assert_eq!(
        invalid(r#"{"worktreeCleanup":{"mode":"sometimes"}}"#).0,
        "worktreeCleanup"
    );
    assert_eq!(
        invalid(r#"{"worktreeCleanup":{"mode":"custom","rules":{"worktreeAfterDays":null}}}"#),
        ("worktreeCleanup.rules.worktreeOnMerge", MISSING)
    );
    assert_eq!(
        invalid(r#"{"projectSettingsOverrides":{"secret-project":{"worktreeCleanup":7}}}"#).0,
        "projectSettingsOverrides.*.worktreeCleanup",
        "the project id never appears in a message"
    );
    assert_eq!(
        invalid(r#"{"sidebarAutoSettleAfterDays":91}"#).0,
        "sidebarAutoSettleAfterDays"
    );
    assert_eq!(
        ok(r#"{"sidebarAutoSettleAfterDays":null}"#).auto_settle_days,
        None
    );
    assert_eq!(
        ok(r#"{"sidebarAutoSettleAfterDays":2.5}"#).auto_settle_days,
        Some(2.5)
    );
    assert_eq!(invalid("[1,2]"), ("settings.json", "is not a JSON object"));
    assert_eq!(invalid("{not json"), ("settings.json", "is not valid JSON"));
}

#[test]
fn secrets_and_other_keys_are_never_kept() {
    let canary = "CANARY-sk-live-0123456789";
    let text = json!({
        "speechToText": {"openaiApiKey": canary, "provider": "openai"},
        "providerInstances": {"x": {"apiKey": canary, "env": {"TOKEN": canary}}},
        "observability": {"otlpTracesUrl": format!("https://{canary}@example.invalid")},
        "projectSettingsOverrides": {"p": {"worktreeCleanup": {"mode": "off"}, "defaultProjectScripts": [{"command": canary}]}},
        "storageCleanup": {"worktreeAfterDays": 30, "logsAfterDays": 7, "browserArtifactsAfterDays": 3},
        "worktreesDirectory": "/srv/trees",
    })
    .to_string();
    let read = parse(&text);
    let debug = format!("{read:?}");
    assert!(!debug.contains("CANARY"), "{debug}");
    assert!(matches!(read, RulesRead::Ok(_)));
}

#[cfg(unix)]
#[test]
fn file_reads_refuse_links_fifos_and_oversize_files() {
    use std::os::unix::fs::symlink;
    let dir = tempdir();
    let data = dir.join("t3");
    std::fs::create_dir_all(data.join("userdata")).unwrap();
    assert_eq!(read(&data), RulesRead::Absent);
    let file = data.join("userdata/settings.json");
    std::fs::write(
        &file,
        r#"{"storageCleanup":{"worktreeSettledAfterDays":3}}"#,
    )
    .unwrap();
    let first = fingerprint(&data);
    assert!(first.is_some());
    assert!(matches!(read(&data), RulesRead::Ok(s) if s.storage.settled_after_days == Some(3)));

    // A linked file and a linked userdata folder are both refused.
    let elsewhere = dir.join("elsewhere.json");
    std::fs::write(&elsewhere, "{}").unwrap();
    std::fs::remove_file(&file).unwrap();
    symlink(&elsewhere, &file).unwrap();
    assert_eq!(
        read(&data),
        RulesRead::Unreadable("symlink not followed".into())
    );
    std::fs::remove_file(&file).unwrap();
    let linked = dir.join("linked");
    std::fs::create_dir_all(dir.join("real/userdata")).unwrap();
    std::fs::write(dir.join("real/userdata/settings.json"), "{}").unwrap();
    std::fs::create_dir_all(&linked).unwrap();
    symlink(dir.join("real/userdata"), linked.join("userdata")).unwrap();
    assert_eq!(
        read(&linked),
        RulesRead::Unreadable("symlink not followed".into())
    );

    // A FIFO does not block and is not a regular file.
    let fifo = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(
        read(&data),
        RulesRead::Unreadable("not a regular file".into())
    );
    std::fs::remove_file(&file).unwrap();

    let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4 * 1024 * 1024));
    std::fs::write(&file, big).unwrap();
    assert_eq!(
        read(&data),
        RulesRead::Unreadable("larger than 4 MiB".into())
    );
    assert_ne!(fingerprint(&data), first);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn tempdir() -> PathBuf {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join(format!(
        "loupe-cleanup-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn managed_roots_follow_t3s_resolution() {
    let settings = CleanupSettings {
        worktrees_directory: "~/trees".into(),
        previous_worktrees_directories: vec![
            "relative".into(),
            "/".into(),
            "/old/../srv/old".into(),
            "".into(),
        ],
        ..CleanupSettings::default()
    };
    let roots = managed_roots(
        &settings,
        Path::new("/h/.t3/worktrees"),
        Some(Path::new("/h")),
    );
    assert_eq!(
        roots,
        [
            PathBuf::from("/h/.t3/worktrees"),
            "/h/trees".into(),
            "/srv/old".into()
        ]
    );
    assert!(custom_root(
        &settings,
        Path::new("/h/.t3/worktrees"),
        Some(Path::new("/h"))
    ));
    assert!(!custom_root(
        &CleanupSettings::default(),
        Path::new("/h/.t3/worktrees"),
        None
    ));
}

fn schema_db(dir: &Path, schema: &str) -> PathBuf {
    let path = dir.join("state.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(schema).unwrap();
    path
}

#[test]
fn thread_facts_read_every_schema_fixture_without_text() {
    let fixtures = [
        include_str!("../../../../checks/fixtures/t3-schema-main-096.sql"),
        include_str!("../../../../checks/fixtures/t3-schema-replayed-083.sql"),
        include_str!("../../../../checks/fixtures/t3-schema-older-074.sql"),
    ];
    for schema in fixtures {
        let dir = tempdir();
        let path = schema_db(&dir, schema);
        let db = Connection::open(&path).unwrap();
        let insert = |id: &str,
                      archived: Option<&str>,
                      deleted: Option<&str>,
                      payload: serde_json::Value| {
            db.execute(
                "INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,archived_at,deleted_at,payload_json)
                 VALUES(?1,'proj','SECRET TITLE','codex','regular','default','2026-11-01T00:00:00.000Z','2026-11-20T00:00:00.000Z',?2,?3,?4)",
                params![id, archived, deleted, payload.to_string()],
            )
            .unwrap();
        };
        insert(
            "live",
            None,
            None,
            json!({"worktreePath": "/w/live", "settledAt": "2026-11-10T00:00:00.000Z", "settledOverride": null}),
        );
        insert(
            "arch",
            Some("2026-11-12T00:00:00.000Z"),
            None,
            json!({"worktreePath": "/w/arch"}),
        );
        insert(
            "gone",
            None,
            Some("2026-11-13T00:00:00.000Z"),
            json!({"worktreePath": "/w/gone", "settledOverride": "active"}),
        );
        insert("none", None, None, json!({"worktreePath": null}));
        db.execute(
            "INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json)
             VALUES('r1','live',1,'codex','completed','2026-11-05T00:00:00.000Z','2026-11-06T00:00:00.000Z','{\"startedAt\":\"2026-11-05T01:00:00.000Z\",\"input\":\"SECRET PROMPT\"}')",
            [],
        )
        .unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let mut facts = read_thread_facts(&path).unwrap();
        facts.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
        assert_eq!(facts.len(), 3);
        let live = facts.iter().find(|f| f.thread_id == "live").unwrap();
        assert_eq!(
            live.last_run_ms,
            crate::model::ms(Some("2026-11-06T00:00:00.000Z"))
        );
        assert_eq!(live.activity_ms(), live.last_run_ms);
        assert!(live.settled());
        let arch = facts.iter().find(|f| f.thread_id == "arch").unwrap();
        assert!(arch.archived && !arch.deleted && !arch.settled());
        assert_eq!(
            arch.activity_ms(),
            crate::model::ms(Some("2026-11-01T00:00:00.000Z"))
        );
        let gone = facts.iter().find(|f| f.thread_id == "gone").unwrap();
        assert!(gone.deleted);
        assert_eq!(gone.settled_override, Some(SettledOverride::Active));
        assert!(!format!("{facts:?}").contains("SECRET"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the read never writes"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
    let dir = tempdir();
    let path = schema_db(
        &dir,
        include_str!("../../../../checks/fixtures/t3-schema-legacy-054.sql"),
    );
    assert_eq!(read_thread_facts(&path).unwrap_err(), "schema");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ── estimate and band ────────────────────────────────────────────────────────

fn row(id: &str, state: ThreadState, worktree: Bytes, build: u64) -> Entry {
    Entry {
        id: id.into(),
        title: id.into(),
        worktree_path: Some(format!("/root/{id}").into()),
        sizes: Sizes {
            worktree,
            build: Bytes::Exact(build),
            scratch: Bytes::Exact(5 * GB),
        },
        state,
        age: None,
        open_target: None,
    }
}

fn facts(
    project: &str,
    ownership: Ownership,
    activity_days: f64,
    settled_days: Option<f64>,
) -> EntryFacts {
    EntryFacts {
        project_id: Some(project.into()),
        ownership,
        idle: true,
        managed: true,
        activity_ms: Some(NOW - activity_days * DAY_MS),
        settled_at_ms: settled_days.map(|d| NOW - d * DAY_MS),
    }
}

fn scene() -> (Inventory, HashMap<String, EntryFacts>) {
    let inventory = Inventory {
        scan: Scan::Finished {
            folders: 6,
            incomplete: 0,
            elapsed: None,
            finished_at: None,
        },
        projects: vec![Project {
            id: "a".into(),
            title: "a".into(),
            threads: vec![row(
                "thread:busy",
                ThreadState::Running,
                Bytes::Exact(9 * GB),
                0,
            )],
        }],
        settled_or_gone: vec![
            row("thread:s10", ThreadState::Settled, Bytes::Exact(2 * GB), GB),
            row("thread:s3", ThreadState::Settled, Bytes::AtLeast(GB), 0),
            row("folder:arch", ThreadState::Gone, Bytes::Exact(3 * GB), 0),
            row("folder:del", ThreadState::Gone, Bytes::Exact(4 * GB), 0),
            row("folder:orphan", ThreadState::Gone, Bytes::Exact(8 * GB), 0),
            row(
                "thread:other",
                ThreadState::Settled,
                Bytes::Exact(6 * GB),
                0,
            ),
        ],
        ..Inventory::default()
    };
    let mut map = HashMap::new();
    map.insert(
        "thread:busy".into(),
        EntryFacts {
            idle: false,
            ..facts("a", Ownership::Active, 40., None)
        },
    );
    // Activity after settling restarts the clock: max(settled 12d, activity 10d) = 10 days.
    map.insert(
        "thread:s10".into(),
        facts("a", Ownership::Settled, 10., Some(12.)),
    );
    map.insert(
        "thread:s3".into(),
        facts("a", Ownership::Settled, 5., Some(3.)),
    );
    map.insert(
        "folder:arch".into(),
        facts("a", Ownership::Archived, 20., Some(8.)),
    );
    map.insert(
        "folder:del".into(),
        facts("a", Ownership::Deleted, 2., None),
    );
    map.insert(
        "folder:orphan".into(),
        EntryFacts {
            project_id: None,
            ..facts("a", Ownership::NoThread, 99., None)
        },
    );
    map.insert(
        "thread:other".into(),
        EntryFacts {
            managed: false,
            ..facts("b", Ownership::Settled, 30., Some(30.))
        },
    );
    (inventory, map)
}

fn projects() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

#[test]
fn off_suggests_settled_seven_days_and_counts_worktree_bytes_only() {
    let (inventory, map) = scene();
    let band = summarize(
        &RulesRead::Absent,
        &inventory,
        Some(&map),
        &projects(),
        false,
        NOW,
    );
    assert_eq!(band.state, CleanupState::Off { defaults: true });
    assert_eq!(band.suggestion, Some(Suggestion::Settled(7)));
    let Estimate::Ready(tally) = band.estimate else {
        panic!("{:?}", band.estimate)
    };
    // s10 (10 days) and the archived thread (settled 8 days, active 20 days ago) are due.
    // Not: busy, the deleted thread (on delete only), the orphan, the folder outside the roots.
    assert_eq!(
        tally.now,
        Count {
            worktrees: 2,
            bytes: 5 * GB,
            partial: false
        }
    );
    // s3 settled 3 days ago, last active 5 days ago: due in 4 days, a lower bound.
    assert_eq!(
        tally.next,
        Some(Next {
            days: 4,
            count: Count {
                worktrees: 1,
                bytes: GB,
                partial: true
            }
        })
    );
    assert!(
        band.entries.values().all(|e| e.due.is_none()),
        "no due claims while rules are off"
    );
    assert_eq!(band.entries["folder:orphan"].ownership, Ownership::NoThread);
}

#[test]
fn auto_settle_off_suggests_inactive_fourteen_days() {
    let (inventory, map) = scene();
    let read = parse(r#"{"sidebarAutoSettleAfterDays":null}"#);
    let band = summarize(&read, &inventory, Some(&map), &projects(), false, NOW);
    assert_eq!(band.state, CleanupState::Off { defaults: false });
    assert_eq!(band.suggestion, Some(Suggestion::Inactive(14)));
    let Estimate::Ready(tally) = band.estimate else {
        panic!()
    };
    // Only the archived thread was inactive for more than 14 days.
    assert_eq!(
        tally.now,
        Count {
            worktrees: 1,
            bytes: 3 * GB,
            partial: false
        }
    );
}

#[test]
fn on_rules_report_due_entries_and_loose_needs_a_gigabyte_more() {
    let (inventory, map) = scene();
    let read =
        parse(r#"{"storageCleanup":{"worktreeSettledAfterDays":3,"worktreeOnDelete":true}}"#);
    let band = summarize(&read, &inventory, Some(&map), &projects(), false, NOW);
    assert_eq!(band.state, CleanupState::On);
    assert_eq!(band.suggestion, None);
    let Estimate::Ready(tally) = band.estimate else {
        panic!()
    };
    // s10, s3 (exactly 3 days after settling but active 5 days ago), archived, deleted.
    assert_eq!(tally.now.worktrees, 4);
    assert_eq!(tally.now.bytes, 10 * GB);
    assert!(tally.now.partial);
    assert_eq!(band.entries["thread:busy"].due, Some(Due::Busy));
    assert_eq!(band.entries["folder:del"].due, Some(Due::Now));
    assert_eq!(band.entries["folder:orphan"].due, Some(Due::Never));
    assert_eq!(band.entries["thread:other"].due, Some(Due::Outside));

    // Inactive after 90 days frees nothing; settled after 7 would free 5 GB more: loose.
    let loose = parse(
        r#"{"storageCleanup":{"worktreeAfterDays":90,"worktreeKeepWhen":"any-local-files"}}"#,
    );
    let band = summarize(&loose, &inventory, Some(&map), &projects(), false, NOW);
    assert_eq!(band.state, CleanupState::Loose);
    assert_eq!(band.suggestion, Some(Suggestion::Settled(7)));
    let Estimate::Ready(tally) = band.estimate else {
        panic!()
    };
    assert_eq!(
        tally.extra,
        Some(Count {
            worktrees: 2,
            bytes: 5 * GB,
            partial: false
        })
    );
    assert!(
        band.build_kept,
        "s10 has build output under keep any local files"
    );
    assert_eq!(band.entries["thread:s10"].due, Some(Due::InDays(80)));

    // Under a gigabyte more is not loose.
    let mut small = inventory.clone();
    for entry in &mut small.settled_or_gone {
        entry.sizes.worktree = Bytes::Exact(100_000_000);
    }
    let band = summarize(&loose, &small, Some(&map), &projects(), false, NOW);
    assert_eq!(band.state, CleanupState::On);
}

#[test]
fn project_overrides_leave_the_suggestion_scope() {
    let (inventory, map) = scene();
    let read = parse(
        r#"{"projectSettingsOverrides":{"a":{"worktreeCleanup":{"mode":"custom","rules":{"worktreeAfterDays":null,"worktreeSettledAfterDays":1,"worktreeOnMerge":false,"worktreeOnDelete":false,"worktreeUnchanged":false}}}}}"#,
    );
    let band = summarize(&read, &inventory, Some(&map), &projects(), false, NOW);
    assert_eq!(band.state, CleanupState::Off { defaults: false });
    assert_eq!(band.own_rules, 1);
    assert!(
        matches!(band.project_rules["a"], ProjectRule::Custom(r) if r.settled_after_days == Some(1))
    );
    let Estimate::Ready(tally) = band.estimate else {
        panic!()
    };
    // Every candidate is in project a, which keeps its own rule.
    assert_eq!(tally.now, Count::default());
    assert_eq!(band.entries["thread:s3"].due, Some(Due::Now));
    // Every known project overridden: no suggestion.
    let all = parse(
        r#"{"projectSettingsOverrides":{"a":{"worktreeCleanup":{"mode":"off"}},"b":{"worktreeCleanup":null}}}"#,
    );
    let band = summarize(&all, &inventory, Some(&map), &projects(), false, NOW);
    assert_eq!(band.suggestion, None);
    assert_eq!(band.own_rules, 2);
}

#[test]
fn unknown_settings_give_no_rules_and_no_suggestion() {
    let (inventory, map) = scene();
    let read = RulesRead::Invalid {
        key: "storageCleanup.worktreeAfterDays",
        constraint: DAYS_1,
    };
    let band = summarize(&read, &inventory, Some(&map), &projects(), false, NOW);
    assert!(matches!(
        band.state,
        CleanupState::Unknown(CleanupProblem::Invalid { .. })
    ));
    assert_eq!(band.global, None);
    assert_eq!(band.suggestion, None);
    assert_eq!(
        band.entries["folder:arch"],
        EntryCleanup {
            ownership: Ownership::Archived,
            due: None
        }
    );
}

#[test]
fn estimate_states_without_sizes_or_times() {
    let (inventory, map) = scene();
    let unavailable = Inventory {
        scan: Scan::Unavailable,
        ..inventory.clone()
    };
    assert_eq!(
        summarize(
            &RulesRead::Absent,
            &unavailable,
            Some(&map),
            &[],
            false,
            NOW
        )
        .estimate,
        Estimate::NoSizes
    );
    let scanning = Inventory {
        scan: Scan::Scanning(Default::default()),
        ..Inventory::default()
    };
    assert_eq!(
        summarize(&RulesRead::Absent, &scanning, None, &[], false, NOW).estimate,
        Estimate::Estimating
    );
    assert_eq!(
        summarize(&RulesRead::Absent, &inventory, None, &[], false, NOW).estimate,
        Estimate::NoTimes
    );
}

#[test]
fn entry_facts_tell_archived_deleted_and_no_thread_apart() {
    let inventory = Inventory {
        projects: vec![Project {
            id: "p".into(),
            title: "p".into(),
            threads: vec![Entry {
                id: "thread:t".into(),
                ..row("t", ThreadState::Done, Bytes::Exact(1), 0)
            }],
        }],
        settled_or_gone: vec![
            Entry {
                worktree_path: Some("/root/a".into()),
                ..row("folder:a", ThreadState::Gone, Bytes::Exact(1), 0)
            },
            Entry {
                worktree_path: Some("/root/d".into()),
                ..row("folder:d", ThreadState::Gone, Bytes::Exact(1), 0)
            },
            Entry {
                worktree_path: Some("/elsewhere/o".into()),
                ..row("folder:o", ThreadState::Gone, Bytes::Exact(1), 0)
            },
        ],
        ..Inventory::default()
    };
    let fact = |id: &str, path: &str, archived, deleted| ThreadFact {
        thread_id: id.into(),
        project_id: "p".into(),
        worktree_path: path.into(),
        created_ms: Some(NOW - 9. * DAY_MS),
        archived,
        deleted,
        settled_at_ms: None,
        settled_override: None,
        last_run_ms: Some(NOW - DAY_MS),
    };
    let facts = vec![
        fact("t", "/root/t", false, false),
        fact("x", "/root/a", true, false),
        fact("y", "/root/d", false, true),
    ];
    let map = entry_facts(
        &inventory,
        &facts,
        &|id| id == "t",
        &[PathBuf::from("/root")],
    );
    assert_eq!(map["thread:t"].ownership, Ownership::Active);
    assert!(map["thread:t"].idle && map["thread:t"].managed);
    assert_eq!(map["thread:t"].activity_ms, Some(NOW - DAY_MS));
    assert_eq!(map["folder:a"].ownership, Ownership::Archived);
    assert_eq!(map["folder:a"].project_id.as_deref(), Some("p"));
    assert_eq!(map["folder:d"].ownership, Ownership::Deleted);
    assert_eq!(map["folder:o"].ownership, Ownership::NoThread);
    assert!(!map["folder:o"].managed);

    let mut inventory = inventory;
    for entry in &mut inventory.settled_or_gone {
        entry.title = format!("unowned {}", entry.title);
    }
    attribute(&mut inventory, &map);
    attribute(&mut inventory, &map);
    let titles: Vec<_> = inventory
        .settled_or_gone
        .iter()
        .map(|e| e.title.as_str())
        .collect();
    assert_eq!(titles, ["folder:a", "folder:d", "unowned folder:o"]);
    assert_eq!(
        inventory
            .settled_projects
            .get("folder:a")
            .map(String::as_str),
        Some("p")
    );
    assert!(!inventory.settled_projects.contains_key("folder:o"));
}
