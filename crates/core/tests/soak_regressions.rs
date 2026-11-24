//! Reductions of what `checks/bench/reader-soak.py` found: the smallest databases and writes that
//! make the incremental reader disagree with the full reader. Each test polls the incremental reader
//! with its self-check on and compares the snapshot with a fresh full read, as the soak does. These
//! tests keep those defects from returning.
mod support;
use support::{Db, stamp};
use loupe_core::{live::Reader, source};
const NOW: f64 = 1_794_484_800_000.;

/// Poll once and demand what the oracle says: the same snapshot as a full read, and no mismatch flag.
fn agrees(reader: &mut Reader, db: &Db, what: &str) -> loupe_core::live::Update {
    let update = reader.poll(NOW).unwrap();
    let full = source::snapshot(&db.path, NOW).unwrap();
    let served = serde_json::to_value(&update.snapshot).unwrap();
    let oracle = serde_json::to_value(&full).unwrap();
    assert!(
        !update.mismatch && served == oracle,
        "{what}: served {:?} ({:?}) disagrees with the full reader: self-check flagged {}, snapshots equal {}",
        update.served,
        update.counters.fallback_reason,
        update.mismatch,
        served == oracle
    );
    update
}

fn ids(snapshot: &loupe_core::model::Snapshot, agent: &str) -> Vec<String> {
    snapshot
        .agents
        .iter()
        .find(|found| found.id == agent)
        .map(|a| a.items.iter().map(|i| i.command.clone()).collect())
        .unwrap_or_default()
}

/// Soak finding 1 (tier m seed 2 step 212, tier real seed 1 step 192): a thread whose activity has no
/// run gets its first run. Its runless items leave the item selection, so a row that the 3,000-item cap
/// had cut comes back in, and the incremental reader does not bring it back.
///
/// `a` has 3,000 runless commands, `b` one newer runless command: 3,001 rows for 3,000 places, so the
/// oldest of `a` is cut. When `b` starts a run, its command leaves the selection and `a`'s oldest
/// returns. The full reader shows it; the incremental one served from its cache does not.
#[test]
fn a_thread_gets_its_first_run_while_the_item_cap_binds() {
    capped_candidates("first", true, false);
    capped_candidates("first", false, false);
}

fn capped_candidates(case: &str, self_check: bool, cold: bool) {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    let tx = db.db.transaction().unwrap();
    for i in 1..=3000 {
        let id = format!("a{i}");
        tx.execute(
            "INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,'command_execution','completed',?,?)",
            rusqlite::params![id, "a", i, stamp(i), format!(r#"{{"input":"cmd-{i}"}}"#)],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('b1','b',1,'command_execution','completed',?,'{\"input\":\"cmd-b\"}')",
        [stamp(3001)],
    )
    .unwrap();
    tx.commit().unwrap();
    db.write("a", 2);
    db.write("b", 3);
    if case != "first" {
        db.db.execute_batch(r#"
            INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
            VALUES('old-b','b',1,'codex','completed','2020-01-01T00:00:00.000Z','{"id":"old-b","threadId":"b","status":"completed"}');
            UPDATE orchestration_v2_projection_turn_items SET run_id='old-b' WHERE thread_id='b';
            INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json)
            VALUES('plan-b','b','old-b',2,'todo_list','completed','2020-01-01T00:00:00.000Z','{"steps":[{"text":"old-plan","status":"pending"}]}');
        "#).unwrap();
    }
    if cold {
        // Preserve the cap's order, but remove the event window as a second
        // reason for keeping these commands after their run loses selection.
        db.db.execute_batch("UPDATE orchestration_v2_projection_turn_items SET updated_at=replace(updated_at,'2026','2020')").unwrap();
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(self_check);
    let first = agrees(&mut reader, &db, "the first poll");
    assert!(!ids(&first.snapshot, "a").iter().any(|c| c == "cmd-1"), "the cap should cut a's oldest command");

    if matches!(case, "archive" | "delete" | "move") {
        let sql = match case {
            "archive" => "UPDATE orchestration_v2_projection_threads SET archived_at='synthetic' WHERE thread_id='b'",
            "delete" => "UPDATE orchestration_v2_projection_threads SET deleted_at='synthetic' WHERE thread_id='b'",
            "move" => "UPDATE orchestration_v2_projection_runs SET thread_id='a',payload_json='{\"id\":\"old-b\",\"threadId\":\"a\",\"status\":\"completed\"}' WHERE run_id='old-b'",
            _ => unreachable!(),
        };
        db.touch("b", 4, sql);
        let after = agrees(&mut reader, &db, case);
        assert_eq!(after.served, loupe_core::live::Served::Incremental);
        // Moving b's run selects it on a: its item remains a candidate.
        assert_eq!(ids(&after.snapshot, "a").iter().any(|c| c == "cmd-1"), case != "move");
        assert_eq!(reader.mismatches(), 0);
        return;
    }
    // b starts its first (or newer latest) run, with an event on b only.
    db.touch(
        "b",
        4,
        r#"INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
           VALUES('rb','b',2,'codex','running','2026-11-12T11:59:00.000Z','{"id":"rb","threadId":"b","status":"running","startedAt":"2026-11-12T11:59:00.000Z"}')"#,
    );
    let after = agrees(&mut reader, &db, "the poll after b's first run");
    assert!(ids(&after.snapshot, "a").iter().any(|c| c == "cmd-1"), "the cut command returns when b's leaves");
    assert_eq!(after.served, loupe_core::live::Served::Incremental);

    if case != "first" {
        assert!(after.snapshot.agents.iter().find(|a| a.id == "b").unwrap().plan.is_none());
        // The old run was evicted by the memory filter. Selecting it again
        // must fetch its row by run id, not depend on a retained key.
        db.touch("b", 5, "DELETE FROM orchestration_v2_projection_runs WHERE run_id='rb'");
        let restored = agrees(&mut reader, &db, "the previous latest run returns");
        assert_eq!(restored.served, loupe_core::live::Served::Incremental);
        assert_eq!(restored.snapshot.agents.iter().find(|a| a.id == "b").unwrap().plan.as_ref().unwrap()[0].text, "old-plan");
        assert!(ids(&restored.snapshot, "b").iter().any(|c| c == "cmd-b"));
        assert!(!ids(&restored.snapshot, "a").iter().any(|c| c == "cmd-1"));
        return;
    }
    // Losing the last run admits the unchanged runless row again. The cache
    // must retain it even when it is outside the event window (the cold variant).
    db.touch("b", 5, "DELETE FROM orchestration_v2_projection_runs WHERE run_id='rb'");
    let restored = agrees(&mut reader, &db, "the poll after b loses its last run");
    assert_eq!(restored.served, loupe_core::live::Served::Incremental);
    assert!(!ids(&restored.snapshot, "a").iter().any(|c| c == "cmd-1"));
    assert!(ids(&restored.snapshot, "b").iter().any(|c| c == "cmd-b"));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn capped_candidates_follow_new_latest_runs_and_active_membership() {
    for case in ["newer", "archive", "delete", "move"] {
        for self_check in [false, true] {
            capped_candidates(case, self_check, false);
        }
    }
}

#[test]
fn cold_capped_candidates_return_after_memory_eviction() {
    for case in ["first", "newer", "archive", "delete", "move"] {
        for self_check in [false, true] {
            capped_candidates(case, self_check, true);
        }
    }
}

/// Soak finding 2 (found by `--include sparse-null-text`): the original reads an item whose payload has
/// no `input` (a command) or no `fileName` (a file change); its `command` and `file` come out null. The
/// Rust full reader fails the whole read instead, "invalid type: null, expected a string", so the
/// dashboard shows an unsupported-schema error for one sparse row. The full reader is the incremental
/// reader's oracle and fallback, so both fail.
fn sparse_item_is_read(kind: &str) {
    let mut db = Db::new();
    db.write("a", 0);
    db.db
        .execute(
            "INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('i1','a',1,?,'completed',?,'{\"status\":\"completed\"}')",
            rusqlite::params![kind, stamp(1)],
        )
        .unwrap();
    db.write("a", 1);
    let full = source::snapshot(&db.path, NOW);
    assert!(full.is_ok(), "the full reader fails on a {kind} item with no text: {:?}", full.err().map(|e| e.to_string()));
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let update = reader.poll(NOW);
    assert!(update.is_ok(), "the incremental reader's first read fails too: {:?}", update.err().map(|e| e.to_string()));
}

#[test]
fn a_command_item_with_no_input_is_read() {
    sparse_item_is_read("command_execution");
}

#[test]
fn a_file_change_item_with_no_file_name_is_read() {
    sparse_item_is_read("file_change");
}
