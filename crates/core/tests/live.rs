mod support;
use std::sync::Arc;
use support::Db;
use t3_agents_core::{
    live::{FallbackReason, Reader, Served},
    model::{Changed, Snapshot},
    source,
};
const NOW: f64 = 1_794_484_800_000.;
fn equal(a: &Snapshot, b: &Snapshot) {
    assert_eq!(
        serde_json::to_value(a).unwrap(),
        serde_json::to_value(b).unwrap()
    );
    for (a, b) in a.agents.iter().zip(&b.agents) {
        assert_eq!(a.open_asks, b.open_asks, "{}", a.id);
        assert_eq!(a.estimated_asks, b.estimated_asks, "{}", a.id);
        assert_eq!(a.storage, b.storage, "{}", a.id);
    }
}
fn poll(
    reader: &mut Reader,
    db: &Db,
    now: f64,
    previous: &mut Option<Snapshot>,
) -> t3_agents_core::live::Update {
    let u = reader.poll(now).unwrap();
    assert_eq!(
        u.counters.fallback_reason.is_some(),
        u.served == Served::Full
    );
    assert!(!u.mismatch);
    assert_eq!(u.counters.mismatches, 0);
    equal(&u.snapshot, &source::snapshot(&db.path, now).unwrap());
    if let Some(p) = previous {
        let Changed::Agents(ids) = &u.changed else {
            panic!("expected precise changes");
        };
        let mut expected = Vec::new();
        for a in &u.snapshot.agents {
            if let Some(b) = p.agents.iter().find(|b| b.id == a.id) {
                let same =
                    serde_json::to_value(&**a).unwrap() == serde_json::to_value(&**b).unwrap()
                        && a.open_asks == b.open_asks
                        && a.estimated_asks == b.estimated_asks
                        && a.storage == b.storage;
                assert_eq!(Arc::ptr_eq(a, b), same);
                assert_eq!(ids.contains(&a.id), !same);
                if !same {
                    expected.push(a.id.clone());
                }
            } else {
                expected.push(a.id.clone());
                assert!(ids.contains(&a.id));
            }
        }
        for b in &p.agents {
            if !u.snapshot.agents.iter().any(|a| a.id == b.id) {
                expected.push(b.id.clone());
                assert!(ids.contains(&b.id));
            }
        }
        expected.sort();
        assert_eq!(
            ids, &expected,
            "Changed::Agents must contain exactly the content differences"
        );
    }
    *previous = Some(u.snapshot.clone());
    u
}

#[test]
fn storage_metadata_refreshes_even_when_the_display_cwd_stays_the_same() {
    use serde_json::json;

    let mut db = Db::new();
    let root = db.path.parent().unwrap().to_owned();
    let worktree = root.join("worktrees/explicit");
    let payload = json!({"worktreePath": worktree, "settledOverride": "settled"});
    db.thread("a", 0, payload.clone());
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let initial = poll(&mut reader, &db, NOW, &mut previous);
    let agent = &initial.snapshot.agents[0];
    assert!(agent.shelf.as_ref().unwrap().settled);
    let storage = agent.storage.as_ref().unwrap();
    assert_eq!(storage.project_id, "p");
    assert_eq!(storage.worktree.as_deref(), worktree.to_str());
    assert_eq!(storage.project_root.as_deref(), Some("/synthetic"));
    let wire = serde_json::to_value(&**agent).unwrap();
    assert!(wire.get("storage").is_none());

    let project_root = root.join("project");
    db.db
        .execute("UPDATE projection_projects SET workspace_root=?", [project_root.to_str().unwrap()])
        .unwrap();
    // A normal changed thread poll loads the updated project projection too.
    db.thread("a", 1, payload);
    let updated = poll(&mut reader, &db, NOW, &mut previous);
    let agent = &updated.snapshot.agents[0];
    assert_eq!(serde_json::to_value(&**agent).unwrap(), wire);
    assert_eq!(
        agent.storage.as_ref().unwrap().project_root.as_deref(),
        project_root.to_str()
    );
    assert_eq!(updated.changed, Changed::Agents(vec!["a".into()]));

    db.thread("a", 2, json!({}));
    let updated = poll(&mut reader, &db, NOW, &mut previous);
    let agent = &updated.snapshot.agents[0];
    assert_eq!(agent.cwd, project_root.to_str().unwrap());
    assert!(agent.storage.as_ref().unwrap().worktree.is_none());
}
#[test]
fn seeded_transactions_and_clock_steps() {
    // Changes the dashboard's tables without an event, which T3 never does. Without a
    // ledger to count, the reader reads every such commit in full; this checks that.
    let mut db = Db::without_ledger();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let mut seed = 12345u64;
    let mut now = NOW;
    for step in 0..600 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let id = format!("thread-{}", seed % 17);
        match seed % 8 {
            0..=2 => db.write(&id, step),
            3 => {
                db.db
                    .execute(
                        "UPDATE projection_projects SET title=?",
                        [format!("Project {step}")],
                    )
                    .unwrap();
            }
            4 => {
                db.db.execute("UPDATE orchestration_v2_projection_threads SET archived_at=CASE WHEN archived_at IS NULL THEN '' ELSE NULL END WHERE thread_id=?",[&id]).unwrap();
            }
            5 => {
                db.db.execute("UPDATE orchestration_v2_projection_threads SET deleted_at=CASE WHEN deleted_at IS NULL THEN '' ELSE NULL END WHERE thread_id=?",[&id]).unwrap();
            }
            6 => now += 1001.,
            _ => now -= 100.,
        }
        poll(&mut reader, &db, now, &mut prev);
    }
    assert_eq!(reader.mismatches(), 0);
}
#[test]
fn unchanged_polls_read_no_projection_rows_and_keep_arcs() {
    let mut db = Db::new();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    let u = poll(&mut reader, &db, NOW + 1000., &mut prev);
    assert_eq!(u.served, Served::Skipped);
    assert_eq!(u.counters.rows_read, 0);
    assert_eq!(u.changed, Changed::Agents(vec![]));
}
#[test]
fn pruning_vacuum_and_demo_schema_fall_back() {
    let mut db = Db::new();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    for sql in [
        "DELETE FROM orchestration_v2_events",
        "VACUUM",
        "DROP TABLE orchestration_v2_events; DROP TABLE orchestration_v2_projection_metadata",
    ] {
        db.db.execute_batch(sql).unwrap();
        assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    }
}
#[test]
fn replacement_and_environment_change() {
    let mut db = Db::new();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    std::fs::write(
        db.path.parent().unwrap().join("environment-id"),
        "synthetic-machine",
    )
    .unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    let mut replacement = Db::new();
    replacement.write("b", 0);
    replacement
        .db
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE")
        .unwrap();
    // Remove old WAL state only after closing its connections.
    drop(reader);
    db.db
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE")
        .unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    std::fs::rename(&replacement.path, &db.path).unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    assert_eq!(prev.unwrap().agents[0].id, "b");
}
#[test]
fn forward_window_and_backwards_clock() {
    let mut db = Db::new();
    db.write("a", 0);
    db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('i','a',1,'error','failed','2026-11-12T12:00:00.000Z','{}')",[]).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        poll(&mut reader, &db, NOW + 10_800_000., &mut prev).served,
        Served::Skipped
    );
    // A canonical stamp expires with no write. The row leaves the window
    // without a full read; stepping the clock backwards still does.
    assert_eq!(
        poll(&mut reader, &db, NOW + 10_800_001., &mut prev).served,
        Served::Incremental
    );
    assert!(prev.as_ref().unwrap().agents[0].events.is_empty());
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    assert_eq!(prev.unwrap().agents[0].events.len(), 1);
}
#[test]
fn worker_delivers_latest_errors_and_stops() {
    let db = Db::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle =
        t3_agents_core::live::spawn(&db.path, std::time::Duration::from_secs(60), move || {
            let _ = tx.send(());
        });
    rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(handle.take().unwrap().is_ok());
    assert!(handle.take().is_none());
    drop(handle);
    assert!(rx.recv_timeout(std::time::Duration::from_secs(1)).is_err());
    let (tx, rx) = std::sync::mpsc::channel();
    let h = t3_agents_core::live::spawn(
        db.path.with_extension("missing"),
        std::time::Duration::from_secs(60),
        move || {
            let _ = tx.send(());
        },
    );
    rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(h.take().unwrap().unwrap_err().kind, "missing");
    drop(h);
}

#[test]
fn global_caps_with_tied_timestamps_match_the_oracle() {
    // Changes the dashboard's tables without an event, which T3 never does. Without a
    // ledger to count, the reader reads every such commit in full; this checks that.
    let mut db = Db::without_ledger();
    db.write("a", 0);
    db.write("b", 1);
    let tx = db.db.transaction().unwrap();
    for i in 0..8200 {
        tx.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,'command_execution','failed','2026-11-12T12:00:00.000Z',?)",rusqlite::params![format!("item-{i}"),if i%2==0 {"a"}else{"b"},i,format!("{{\"title\":\"Synthetic {i}\",\"input\":\"synthetic command\"}}")]).unwrap();
    }
    tx.commit().unwrap();
    let mut r = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut p = None;
    let u = poll(&mut r, &db, NOW, &mut p);
    assert_eq!(
        u.snapshot
            .agents
            .iter()
            .map(|a| a.items.len())
            .sum::<usize>(),
        3000
    );
    assert_eq!(
        u.snapshot
            .agents
            .iter()
            .map(|a| a.events.len())
            .sum::<usize>(),
        8000
    );
    db.db.execute("UPDATE orchestration_v2_projection_turn_items SET status='completed' WHERE turn_item_id='item-8199'",[]).unwrap();
    poll(&mut r, &db, NOW, &mut p);
    poll(&mut r, &db, NOW + 10_800_001., &mut p);
}

#[test]
fn polling_a_writer_returns_coherent_snapshots() {
    // Changes the dashboard's tables without an event, which T3 never does. Without a
    // ledger to count, the reader reads every such commit in full; this checks that.
    let mut db = Db::without_ledger();
    db.write("a", 0);
    db.write("b", 1);
    let path = db.path.clone();
    let writer = std::thread::spawn(move || {
        let mut c = rusqlite::Connection::open(path).unwrap();
        c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        for i in 0..200 {
            let tx = c.transaction().unwrap();
            for id in ["a", "b"] {
                tx.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title',?) WHERE thread_id=?",rusqlite::params![format!("Version {i}"),id]).unwrap();
            }
            tx.commit().unwrap();
        }
    });
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    for _ in 0..200 {
        let u = reader.poll(NOW).unwrap();
        assert!(!u.mismatch);
        let a = &u.snapshot.agents;
        // The pre-writer state has distinct initial titles; every later state
        // changes both titles in one transaction.
        if a[0].title.starts_with("Version") {
            assert_eq!(a[0].title, a[1].title);
        }
    }
    writer.join().unwrap();
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn a_commit_between_the_probe_and_the_read_is_not_taken_for_a_quiet_clock() {
    // The poll first asks SQLite whether anything was written, then opens its read
    // transaction. A commit can land between the two. The read then holds that commit
    // although the probe said nothing changed, and the poll must not answer from what it
    // already had as if only the clock had moved. Like the test above, the commit changes
    // the dashboard's tables without an event and there is no ledger to count, so only a
    // full read can be right.
    let mut db = Db::without_ledger();
    db.write("a", 0);
    db.write("b", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    poll(&mut reader, &db, NOW, &mut previous);
    poll(&mut reader, &db, NOW, &mut previous);
    let path = db.path.clone();
    reader.between_probe_and_read(move || {
        let writer = rusqlite::Connection::open(path).unwrap();
        writer.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Written in between') WHERE thread_id='a'", []).unwrap();
    });
    // The clock moves too, so the poll cannot simply repeat its last answer.
    let update = poll(&mut reader, &db, NOW + 1_000., &mut previous);
    assert_eq!(update.served, Served::Full);
    assert!(update.snapshot.agents.iter().any(|a| a.title == "Written in between"));
    // The reader remembers the probe's answer, the older of the two, so the next poll
    // rereads once more than it needs to. The one after that reads nothing in full.
    poll(&mut reader, &db, NOW + 2_000., &mut previous);
    let settled = poll(&mut reader, &db, NOW + 3_000., &mut previous);
    assert_ne!(settled.served, Served::Full, "{:?}", settled.counters.fallback_reason);
}

#[test]
fn noncanonical_cutoffs_force_forward_rereads() {
    let mut db = Db::new();
    db.write("a", 0);
    db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('i','a',1,'error','failed','2026-11-12T12:00:00Z','{}')",[]).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).served,
        Served::Skipped
    );
    assert_eq!(
        poll(&mut reader, &db, NOW + 1., &mut prev).served,
        Served::Full
    );
    poll(&mut reader, &db, NOW + 10_800_001., &mut prev);
}

#[test]
fn missing_database_is_never_created_and_clock_is_validated() {
    let db = Db::new();
    let path = db.path.with_extension("missing");
    assert_eq!(Reader::open(&path).err().unwrap().kind, "missing");
    assert!(!path.exists());
    let mut reader = Reader::open(&db.path).unwrap();
    assert!(reader.poll(f64::NAN).is_err());
    assert!(reader.poll(f64::INFINITY).is_err());
    assert!(reader.poll(NOW).is_ok());
}

#[test]
fn event_suffix_tracks_bursts_and_project_only_commits() {
    // Changes the dashboard's tables without an event, which T3 never does. Without a
    // ledger to count, the reader reads every such commit in full; this checks that.
    let mut db = Db::without_ledger();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let initial = poll(&mut reader, &db, NOW, &mut previous);
    assert!(!initial.counters.feed_complete);
    let tx = db.db.transaction().unwrap();
    for (event, thread) in [("burst-1", "a"), ("burst-2", "b"), ("burst-3", "a")] {
        tx.execute("INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) VALUES(?,?,'synthetic','','{}')", [event,thread]).unwrap();
    }
    tx.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Burst') WHERE thread_id='a'",[]).unwrap();
    tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events) WHERE projection_name='thread-projections'",[]).unwrap();
    tx.commit().unwrap();
    let burst = poll(&mut reader, &db, NOW, &mut previous);
    assert!(burst.counters.feed_complete);
    assert_eq!(burst.counters.changed_threads, 2);
    assert_eq!(burst.counters.event_rows_read, 5); // tip, anchor, three events
    assert_eq!(burst.served, Served::Incremental);
    db.db
        .execute("UPDATE projection_projects SET title='New project'", [])
        .unwrap();
    let project = poll(&mut reader, &db, NOW, &mut previous);
    assert!(!project.counters.feed_complete);
    assert_eq!(project.served, Served::Full);
    db.write("a", 10);
    assert!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .feed_complete
    );
}

#[test]
fn uncertain_event_feeds_preserve_oracle_fallback_and_rebaseline() {
    for (mutation, reason) in [
        (
            "DELETE FROM orchestration_v2_events WHERE sequence=1",
            FallbackReason::MissingAnchor,
        ),
        (
            "DELETE FROM orchestration_v2_events WHERE sequence=2",
            FallbackReason::EventGap,
        ),
        (
            "UPDATE orchestration_v2_projection_metadata SET last_sequence=99",
            FallbackReason::WatermarkMismatch,
        ),
        (
            "DELETE FROM orchestration_v2_projection_metadata",
            FallbackReason::MissingMetadata,
        ),
        (
            "INSERT INTO orchestration_v2_projection_metadata VALUES('events-pruned-through',1,2,'')",
            FallbackReason::PruningMarkerChanged,
        ),
        (
            "DROP TABLE orchestration_v2_events",
            FallbackReason::SchemaChanged,
        ),
    ] {
        let mut db = Db::new();
        db.write("a", 0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut previous = None;
        poll(&mut reader, &db, NOW, &mut previous);
        db.write("b", 1);
        db.write("c", 2);
        db.db.execute_batch(mutation).unwrap();
        let update = poll(&mut reader, &db, NOW, &mut previous);
        assert!(!update.counters.feed_complete, "{mutation}");
        assert_eq!(update.counters.fallback_reason, Some(reason), "{mutation}");
        assert_eq!(update.served, Served::Full);
        assert_eq!(update.counters.changed_threads, 0);
    }
}

#[test]
fn cache_staging_follows_the_feed_and_rebaselines_after_vacuum() {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let first = poll(&mut reader, &db, NOW, &mut previous);
    assert!(first.counters.cache_ready && first.counters.cache_full);
    assert_eq!(first.counters.cache_rows_read, 3);
    db.write("b", 2);
    let changed = poll(&mut reader, &db, NOW, &mut previous);
    assert!(changed.counters.cache_ready && !changed.counters.cache_full);
    assert_eq!(changed.counters.cache_rows_read, 2);
    assert_eq!(changed.served, Served::Incremental);
    // VACUUM increments schema_version, even when a subsequent event makes the
    // feed otherwise look complete. Cached rowids must not cross that boundary.
    db.db.execute_batch("VACUUM").unwrap();
    db.write("a", 3);
    let vacuum = poll(&mut reader, &db, NOW, &mut previous);
    assert!(vacuum.counters.cache_ready && vacuum.counters.cache_full);
    assert!(!vacuum.counters.feed_complete);
    db.db
        .execute("UPDATE projection_projects SET title='Updated project'", [])
        .unwrap();
    db.write("b", 4);
    let project = poll(&mut reader, &db, NOW, &mut previous);
    assert!(project.counters.cache_ready && !project.counters.cache_full);
    assert_eq!(project.counters.cache_rows_read, 2);
}

#[test]
fn schemas_without_implicit_rowids_keep_the_oracle_fallback() {
    let mut db = Db::new();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    assert!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .cache_ready
    );
    db.db
        .execute_batch(
            "ALTER TABLE orchestration_v2_projection_nodes RENAME TO saved_nodes;
         CREATE TABLE orchestration_v2_projection_nodes(
             node_id TEXT PRIMARY KEY, thread_id TEXT, parent_node_id TEXT, run_id TEXT
         ) WITHOUT ROWID;",
        )
        .unwrap();
    let unsupported = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(unsupported.served, Served::Full);
    assert!(!unsupported.counters.cache_ready);
    db.db
        .execute_batch(
            "DROP TABLE orchestration_v2_projection_nodes;
         ALTER TABLE saved_nodes RENAME TO orchestration_v2_projection_nodes;",
        )
        .unwrap();
    let restored = poll(&mut reader, &db, NOW, &mut previous);
    assert!(restored.counters.cache_ready && restored.counters.cache_full);
}

#[test]
fn cached_content_and_unique_caps_are_served_without_oracle_rows() {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    let mut r = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut r, &db, NOW, &mut prev);
    let tx = db.db.transaction().unwrap();
    for i in 0..8200 {
        let stamp = format!(
            "2026-11-12T12:{:02}:{:02}.{:03}Z",
            i / 60000,
            i / 1000 % 60,
            i % 1000
        );
        tx.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,'command_execution','running',?,?)", rusqlite::params![format!("i{i}"), if i%2==0 {"a"} else {"b"},i,stamp,format!(r#"{{"input":"command {i}"}}"#)]).unwrap();
    }
    tx.commit().unwrap();
    // Record both owners in the same event suffix before serving the new rows.
    db.write("a", 2);
    db.write("b", 3);
    let u = poll(&mut r, &db, NOW, &mut prev);
    assert_eq!(u.served, Served::Incremental);
    assert_eq!(u.counters.rows_read, u.counters.cache_rows_read);
    assert_eq!(
        u.snapshot
            .agents
            .iter()
            .map(|a| a.items.len())
            .sum::<usize>(),
        3000
    );
    assert_eq!(
        u.snapshot
            .agents
            .iter()
            .map(|a| a.events.len())
            .sum::<usize>(),
        8000
    );
    // A write to b displaces a's oldest selected row at both global caps.
    // a has no event and none of its SQL input rows changed.
    let a_before = u
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "a")
        .unwrap()
        .clone();
    db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('cap-new','b',9000,'command_execution','failed','2026-11-12T12:01:00.000Z','{\"input\":\"cap newcomer\"}')", []).unwrap();
    db.write("b", 104);
    let displaced = poll(&mut r, &db, NOW, &mut prev);
    assert_eq!(displaced.served, Served::Incremental);
    let a_after = displaced
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "a")
        .unwrap();
    assert!(!Arc::ptr_eq(&a_before, a_after));
    assert_eq!(a_before.items.len(), a_after.items.len() + 1);
    assert_eq!(a_before.events.len(), a_after.events.len() + 1);
    for step in 4..104 {
        db.db.execute("UPDATE orchestration_v2_projection_turn_items SET status='completed' WHERE turn_item_id=?", [format!("i{}", 8199-step)]).unwrap();
        db.write(if (8199 - step) % 2 == 0 { "a" } else { "b" }, step);
        let u = poll(&mut r, &db, NOW + step as f64, &mut prev);
        assert_eq!(u.served, Served::Incremental);
        assert_eq!(u.counters.rows_read, u.counters.cache_rows_read);
    }
    // A clock-only step removes b's events without a write to b or a.
    let before = prev
        .as_ref()
        .unwrap()
        .agents
        .iter()
        .find(|a| a.id == "b")
        .unwrap()
        .clone();
    let expired = poll(&mut r, &db, NOW + 10_860_001., &mut prev);
    let after = expired
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "b")
        .unwrap();
    assert!(!before.events.is_empty());
    assert!(after.events.is_empty());
    assert!(!Arc::ptr_eq(&before, after));
    assert_eq!(r.mismatches(), 0);
}

#[test]
fn both_tied_cap_cuts_use_full_fallback() {
    let mut db = Db::new();
    db.write("b", 0);
    db.write("a", 1);
    let mut r = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut r, &db, NOW, &mut prev);
    let tx = db.db.transaction().unwrap();
    for i in 0..8200 {
        tx.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,0,'command_execution','failed','2026-11-12T12:00:00.000Z',?)", rusqlite::params![format!("i{i}"), if i%2==0 {"b"} else {"a"}, format!(r#"{{"input":"command {i}"}}"#)]).unwrap();
    }
    tx.commit().unwrap();
    db.write("a", 2);
    db.write("b", 3);
    let u = poll(&mut r, &db, NOW, &mut prev);
    assert_eq!(u.served, Served::Incremental, "{:?}", u.counters.fallback_reason);
    assert_eq!(u.counters.rows_read, u.counters.cache_rows_read);
    // Real-schema plan: runless MULTI-INDEX OR scans owners lexically;
    // the stable temporary sorter keeps encounter order for equal keys.
    // Insertion starts on b, so b has the lower rowids. Thread order keeps a.
    let a = u.snapshot.agents.iter().find(|a| a.id == "a").unwrap();
    let b = u.snapshot.agents.iter().find(|a| a.id == "b").unwrap();
    assert_eq!((a.items.len(), b.items.len()), (3000, 0));
    assert_eq!((a.events.len(), b.events.len()), (4100, 3900));
    // An alternate legal index changes the event encounter order. It cannot be
    // assumed to be the historical plan simply because all columns still exist.
    db.db.execute_batch("CREATE INDEX alternate_items ON orchestration_v2_projection_turn_items(thread_id,updated_at DESC,ordinal DESC)").unwrap();
    db.write("b", 4);
    assert_eq!(poll(&mut r, &db, NOW, &mut prev).served, Served::Full);
}

#[test]
fn nullable_owner_in_an_alternate_schema_disables_the_cache() {
    let mut db = Db::new();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    poll(&mut reader, &db, NOW, &mut previous);
    db.db.execute_batch("DROP TABLE orchestration_v2_projection_nodes;
        CREATE TABLE orchestration_v2_projection_nodes(node_id TEXT, thread_id TEXT, parent_node_id TEXT, run_id TEXT);
        INSERT INTO orchestration_v2_projection_nodes VALUES('unowned',NULL,NULL,NULL);").unwrap();
    let u = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(u.served, Served::Full);
    assert!(!u.counters.cache_ready);
    db.write("a", 1);
    let u = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(u.served, Served::Full);
    assert!(!u.counters.cache_ready);
    assert_eq!(
        u.counters.fallback_reason,
        Some(FallbackReason::CacheUnavailable)
    );
}

#[test]
fn fallback_reasons_describe_baseline_feed_and_clock_rules() {
    // Changes the dashboard's tables without an event, which T3 never does. Without a
    // ledger to count, the reader reads every such commit in full; this checks that.
    let mut db = Db::without_ledger();
    db.write("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .fallback_reason,
        Some(FallbackReason::InitialBaseline)
    );
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .fallback_reason,
        None
    );
    db.write("a", 1);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .fallback_reason,
        None
    );
    db.db
        .execute("UPDATE projection_projects SET title='changed'", [])
        .unwrap();
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .fallback_reason,
        Some(FallbackReason::NoNewEvents)
    );
    assert_eq!(
        poll(&mut reader, &db, NOW - 1., &mut previous)
            .counters
            .fallback_reason,
        Some(FallbackReason::BackwardsClock)
    );
    db.db
        .execute("DELETE FROM orchestration_v2_projection_metadata", [])
        .unwrap();
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut previous)
            .counters
            .fallback_reason,
        Some(FallbackReason::MissingMetadata)
    );
}

#[test]
fn subagent_encounter_order_tracks_index_slices_and_scans() {
    use rusqlite::params;
    use serde_json::json;
    let mut db = Db::new();
    // Neither owner insertion nor rowid order agrees with the composite index.
    for (n, owner) in ["z", "a", "m"].iter().enumerate() {
        db.write(owner, n as u64);
        for i in (0..12).rev() {
            let id = format!("{owner}-{i:02}");
            let start = if i % 3 == 0 {
                "2026-11-12T10:00:00Z"
            } else {
                "2026-11-12T11:00:00Z"
            };
            db.db.execute("INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,started_at,updated_at,payload_json) VALUES(?,?,'parent','codex','synthetic','running',?,'',?)",
                params![id, owner, start, json!({"id":id,"threadId":owner,"parentNodeId":"parent","status":"running","title":id,"startedAt":start}).to_string()]).unwrap();
        }
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    for step in 10..18 {
        // Exercise refresh, equal starts, deletion and replacement of slices.
        db.db.execute("UPDATE orchestration_v2_projection_subagents SET started_at='2026-11-12T09:00:00Z' WHERE thread_id='m' AND subagent_id LIKE '%1'", []).unwrap();
        db.write("m", step);
        let u = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(u.served, Served::Incremental);
    }
    // Schema invalidation must discard both the plan and the old positions.
    db.db
        .execute_batch("DROP INDEX orchestration_v2_projection_subagents_thread_idx")
        .unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.write("z", 20);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).served,
        Served::Incremental
    );
    db.db.execute_batch("CREATE INDEX synthetic_sub_order ON orchestration_v2_projection_subagents(thread_id, started_at DESC, subagent_id DESC)").unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.db.execute("DELETE FROM orchestration_v2_projection_subagents WHERE thread_id='a' AND started_at='2026-11-12T10:00:00Z'", []).unwrap();
    db.write("a", 21);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).served,
        Served::Incremental
    );
    // A covering oracle index is deliberately outside the accepted shapes.
    db.db.execute_batch("DROP INDEX synthetic_sub_order; CREATE INDEX synthetic_sub_cover ON orchestration_v2_projection_subagents(thread_id,payload_json)").unwrap();
    poll(&mut reader, &db, NOW, &mut prev);
    db.write("a", 22);
    let u = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        u.counters.fallback_reason,
        Some(FallbackReason::MultipleSubagents)
    );
}

#[test]
fn request_encounter_order_tracks_predicated_index_slices_and_scans() {
    use rusqlite::params;
    let mut db = Db::new();
    // All statuses, repeated timestamps, reversed keys and nonlexical owners.
    // Kinds differ so reordering cannot hide behind equal output strings.
    for (n, owner) in ["z", "a", "m"].iter().enumerate() {
        db.write(owner, n as u64);
        for i in (0..16).rev() {
            let status = ["waiting", "pending", "open", "requested"][i % 4];
            db.db.execute("INSERT INTO orchestration_v2_projection_runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json) VALUES(?,?,'parent',?,?,?,NULL,'{}')", params![format!("{owner}-{i:02}"),owner,format!("kind-{owner}-{i}"),status,if i%2==0 {"2026-11-12T10:00:00Z"} else {"2026-11-12T11:00:00Z"}]).unwrap();
        }
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    for step in 10..15 {
        db.db.execute("UPDATE orchestration_v2_projection_runtime_requests SET status=CASE status WHEN 'open' THEN 'pending' ELSE 'open' END WHERE thread_id='m'",[]).unwrap();
        db.write("m", step);
        let u = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(u.served, Served::Incremental);
        assert_eq!(
            u.snapshot
                .agents
                .iter()
                .map(|a| a.requests.len())
                .sum::<usize>(),
            48
        );
    }
    db.db.execute_batch("DROP INDEX orchestration_v2_projection_runtime_requests_thread_status_idx; DROP INDEX orchestration_v2_projection_requests_recovery_idx").unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.write("a", 20);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).served,
        Served::Incremental
    );
    // Resolution removes rows from the cached slice; deletion and owner moves
    // must also preserve the remaining encounter order.
    db.db.execute_batch("CREATE INDEX synthetic_request_order ON orchestration_v2_projection_runtime_requests(thread_id, created_at DESC, runtime_request_id DESC)").unwrap();
    poll(&mut reader, &db, NOW, &mut prev);
    db.db.execute_batch("UPDATE orchestration_v2_projection_runtime_requests SET resolved_at='done' WHERE runtime_request_id='a-01'; DELETE FROM orchestration_v2_projection_runtime_requests WHERE runtime_request_id='a-02'; UPDATE orchestration_v2_projection_runtime_requests SET thread_id='a' WHERE runtime_request_id='m-03'").unwrap();
    db.write("a", 21);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).served,
        Served::Incremental
    );
    // Only destination signalled: rowid ownership must evict the source row.
    db.db.execute_batch("DROP INDEX synthetic_request_order; CREATE INDEX synthetic_request_cover ON orchestration_v2_projection_runtime_requests(thread_id,status,resolved_at,node_id,kind,created_at)").unwrap();
    poll(&mut reader, &db, NOW, &mut prev);
    db.write("z", 22);
    let u = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        u.counters.fallback_reason,
        Some(FallbackReason::MultipleRequests)
    );
}

#[test]
fn event_timestamp_ties_follow_proved_scan_order_even_at_the_cap() {
    use rusqlite::params;
    for count in [36, 8200] {
        let mut db = Db::new();
        for (step, owner) in ["z", "a", "m"].iter().enumerate() {
            db.write(owner, step as u64);
        }
        let tx = db.db.transaction().unwrap();
        for i in 0..count {
            // Interleaved owners and permuted run keys make index order differ
            // from insertion/rowid order, within owners and across owners.
            tx.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,?, 'command_execution','failed',?,?)",
                params![format!("event-{i}"), ["z","a","m"][i%3],
                    format!("unselected-{:05}", (i*17)%count), i as i64,
                    if i < 12 { "2026-11-12T12:00:00.001Z" } else { "2026-11-12T12:00:00.000Z" },
                    format!(r#"{{"input":"synthetic event {i}"}}"#)]).unwrap();
        }
        tx.commit().unwrap();
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        poll(&mut reader, &db, NOW + 1., &mut prev);
        // Default index, alternate descending suffix index, then full scan.
        for shape in 0..3 {
            if shape == 1 {
                db.db.execute_batch("CREATE INDEX synthetic_event_order ON orchestration_v2_projection_turn_items(thread_id, ordinal DESC)").unwrap();
                assert_eq!(
                    poll(&mut reader, &db, NOW + 1., &mut prev).served,
                    Served::Full
                );
            } else if shape == 2 {
                let indexes: Vec<String> = db.db.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='orchestration_v2_projection_turn_items' AND sql IS NOT NULL").unwrap()
                    .query_map([], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
                for index in indexes {
                    db.db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
                }
                poll(&mut reader, &db, NOW + 1., &mut prev);
            }
            for step in 0..3 {
                db.db.execute("UPDATE orchestration_v2_projection_turn_items SET run_id=?,ordinal=ordinal+1 WHERE turn_item_id='event-20'",
                    [format!("unselected-{}", 10-step)]).unwrap();
                db.write("m", (100 + shape * 10 + step) as u64);
                let u = poll(&mut reader, &db, NOW + 1., &mut prev);
                assert_eq!(
                    u.served,
                    Served::Incremental,
                    "{count}/{shape}/{step}: {:?}",
                    u.counters.fallback_reason
                );
                assert_eq!(
                    u.snapshot
                        .agents
                        .iter()
                        .map(|a| a.events.len())
                        .sum::<usize>(),
                    count.min(8000)
                );
                assert_eq!(u.counters.rows_read, u.counters.cache_rows_read);
            }
        }
        // Inclusive lower bound, then expiry without any write. Canonical
        // stamps drop out of the window incrementally; the .001Z rows remain.
        let u = poll(&mut reader, &db, NOW + 10_800_000., &mut prev);
        assert_eq!(
            u.snapshot
                .agents
                .iter()
                .map(|a| a.events.len())
                .sum::<usize>(),
            count.min(8000)
        );
        let u = poll(&mut reader, &db, NOW + 10_800_001., &mut prev);
        assert_eq!(u.served, Served::Incremental);
        assert_eq!(u.counters.fallback_reason, None);
        assert_eq!(
            u.snapshot
                .agents
                .iter()
                .map(|a| a.events.len())
                .sum::<usize>(),
            12
        );
        db.write("a", 999);
        assert_eq!(
            poll(&mut reader, &db, NOW + 10_800_001., &mut prev).served,
            Served::Incremental
        );
        let u = poll(&mut reader, &db, NOW + 10_800_002., &mut prev);
        assert!(u.snapshot.agents.iter().all(|a| a.events.is_empty()));
    }
}

#[test]
fn event_timestamp_ties_reject_a_different_owner_scan() {
    let mut db = Db::new();
    db.write("a", 0);
    db.db.execute_batch("CREATE INDEX synthetic_event_range ON orchestration_v2_projection_turn_items(thread_id,updated_at);
        INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json)
        VALUES('one','a','unselected',0,'command_execution','failed','2026-11-12T12:00:00.000Z','{\"input\":\"synthetic\"}'),
        ('two','a','unselected',1,'command_execution','failed','2026-11-12T12:00:00.000Z','{\"input\":\"synthetic\"}')").unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.write("a", 1);
    let u = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        u.counters.fallback_reason,
        Some(FallbackReason::EventTimestampTie)
    );
}

#[test]
fn poll_profile_is_opt_in_and_excludes_the_canary() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut db = Db::new();
    db.write("profile-thread", 1);
    for enabled in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_t3-agents-core"));
        command
            .args(["--bench-reader-poll", "--db"])
            .arg(&db.path)
            .args(["--now-ms", "1794484800000", "--self-check"])
            .env_remove("T3_AGENTS_READER_POLL_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if enabled {
            command.env("T3_AGENTS_READER_POLL_PROFILE", "1");
        }
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(b"\n\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["served"], "Full");
        assert_eq!(rows[1]["served"], "Skipped");
        for row in &rows {
            assert_eq!(row["mismatches"], 0);
            assert_eq!(!row["profile"].is_null(), enabled);
        }
        if enabled {
            assert!(
                rows[0]["profile"]["phases_ns"]["assembly"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
            // Even a skipped primary poll runs the full canary. Its assembly
            // must not be attributed to the primary reader's phase report.
            assert!(rows[1]["profile"]["phases_ns"].get("assembly").is_none());
        }
    }
}

#[test]
fn archiving_a_thread_drops_its_events_from_the_cap() {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    let tx = db.db.transaction().unwrap();
    // a holds the oldest event, just outside the 8,000 cap. b holds the cap.
    // Archiving b must let a's event in. Leaving b's rows indexed hides it.
    for i in 0..8001 {
        let owner = if i == 0 { "a" } else { "b" };
        let stamp = format!(
            "2026-11-12T12:{:02}:{:02}.{:03}Z",
            i / 60_000,
            i / 1000 % 60,
            i % 1000
        );
        tx.execute(
            "INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,'command_execution','completed',?,?)",
            rusqlite::params![format!("i{i}"), owner, i, stamp, format!(r#"{{"input":"command {i}"}}"#)],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    db.write("a", 2);
    db.write("b", 3);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        first
            .snapshot
            .agents
            .iter()
            .find(|a| a.id == "a")
            .unwrap()
            .events
            .len(),
        0
    );
    assert_eq!(
        first
            .snapshot
            .agents
            .iter()
            .find(|a| a.id == "b")
            .unwrap()
            .events
            .len(),
        8000
    );
    db.db
        .execute(
            "UPDATE orchestration_v2_projection_threads SET archived_at='synthetic' WHERE thread_id='b'",
            [],
        )
        .unwrap();
    db.write("b", 4);
    let archived = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(archived.served, Served::Incremental);
    assert!(archived.snapshot.agents.iter().all(|a| a.id != "b"));
    assert_eq!(
        archived
            .snapshot
            .agents
            .iter()
            .find(|a| a.id == "a")
            .unwrap()
            .events
            .len(),
        1
    );
    // i0 was outside both caps, so its command was not cached. Archiving b
    // pulls it into the item cap and the text read has to supply "command 0".
    let entered = archived
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "a")
        .unwrap();
    assert_eq!(
        entered.items.iter().map(|item| item.command.as_str()).collect::<Vec<_>>(),
        vec!["command 0"]
    );
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn untouched_child_changes_when_its_parent_leaves_the_active_structure() {
    let mut db = Db::new();
    db.write("parent", 0);
    db.write("child", 1);
    db.write("other", 2);
    db.db.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.lineage',json('{\"relationshipToParent\":\"subagent\",\"parentThreadId\":\"parent\"}')) WHERE thread_id='child'", []).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    let child = first
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "child")
        .unwrap();
    assert_eq!(child.parent.as_deref(), Some("parent"));
    db.db.execute("UPDATE orchestration_v2_projection_threads SET archived_at='synthetic' WHERE thread_id='parent'", []).unwrap();
    // A burst signals only the changed owners; child's rows stay untouched.
    db.write("parent", 3);
    db.write("other", 4);
    let changed = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(changed.served, Served::Incremental);
    let next_child = changed
        .snapshot
        .agents
        .iter()
        .find(|a| a.id == "child")
        .unwrap();
    assert!(next_child.parent.is_none());
    assert!(!Arc::ptr_eq(child, next_child));
    let stable = poll(&mut reader, &db, NOW + 1., &mut prev);
    assert!(Arc::ptr_eq(
        next_child,
        stable
            .snapshot
            .agents
            .iter()
            .find(|a| a.id == "child")
            .unwrap()
    ));
}

#[test]
fn a_burst_rebuilds_only_the_threads_it_touches() {
    let mut db = Db::new();
    for i in 0..40 {
        db.write(&format!("t{i:02}"), i);
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    let tx = db.db.transaction().unwrap();
    for i in 0..12 {
        let id = format!("t{i:02}");
        tx.execute(
            "INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) VALUES(?,?,'synthetic','','{}')",
            rusqlite::params![format!("burst-{i}"), id],
        )
        .unwrap();
        tx.execute(
            "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title',?) WHERE thread_id=?",
            rusqlite::params![format!("Burst {i}"), id],
        )
        .unwrap();
    }
    tx.execute(
        "UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events) WHERE projection_name='thread-projections'",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    let u = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(u.served, Served::Incremental);
    assert_eq!(u.counters.changed_threads, 12);
    assert_eq!(u.counters.fallback_reason, None);
    let Changed::Agents(ids) = &u.changed else {
        panic!("expected precise changes");
    };
    let mut expected: Vec<_> = (0..12).map(|i| format!("t{i:02}")).collect();
    expected.sort();
    assert_eq!(ids, &expected);
}

fn plan_text(snapshot: &Snapshot, id: &str) -> Option<String> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .and_then(|agent| agent.plan.as_ref())
        .map(|steps| steps[0].text.clone())
}

/// Tied `todo_list` rows. Insertion order, rowid order and `(run_id, ordinal, rowid)`
/// order are three different sequences. The visible step is the one the oracle keeps.
fn insert_tied_plans(db: &Db) {
    let tie = "2026-11-12T12:00:00.000Z";
    // (id, thread, run, ordinal, rowid, text, node). Scrambled on purpose.
    let rows = [
        ("p-z-dup", "z", "run-z", 1_i64, 80_i64, "dup-z", None),
        ("p-s-dup", "a", "run-s", 1, 50, "dup-s", Some("s")),
        ("p-a-win", "a", "run-a", 1, 25, "win-a", None),
        ("p-m-low", "m", "run-m", 7, 3, "late-m", None),
        ("p-z-win", "z", "run-z", 1, 15, "win-z", None),
        ("p-a-low", "a", "run-a", 8, 2, "late-a", None),
        ("p-m-dup", "m", "run-m", 1, 60, "dup-m", None),
        ("p-s-win", "a", "run-s", 1, 45, "win-s", Some("s")),
        ("p-z-low", "z", "run-z", 9, 1, "late-z", None),
        ("p-a-dup", "a", "run-a", 1, 70, "dup-a", None),
        ("p-m-win", "m", "run-m", 1, 35, "win-m", None),
        ("p-s-low", "a", "run-s", 6, 4, "late-s", Some("s")),
    ];
    for (id, thread, run, ordinal, rowid, text, node) in rows {
        db.db
            .execute(
                "INSERT INTO orchestration_v2_projection_turn_items(rowid,turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,?,?,?,'todo_list','completed',?,?)",
                rusqlite::params![
                    rowid,
                    id,
                    thread,
                    run,
                    node,
                    ordinal,
                    tie,
                    format!(r#"{{"steps":[{{"text":"{text}","status":"pending"}}]}}"#)
                ],
            )
            .unwrap();
    }
}

fn seed_plan_threads(db: &mut Db) {
    for (step, owner) in ["z", "a", "m"].iter().enumerate() {
        db.write(owner, step as u64);
        db.db
            .execute(
                "INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json) VALUES(?,?,1,'codex','running','2026-11-12T12:00:00.000Z',?)",
                rusqlite::params![
                    format!("run-{owner}"),
                    owner,
                    format!(r#"{{"id":"run-{owner}","threadId":"{owner}","status":"running"}}"#)
                ],
            )
            .unwrap();
    }
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('s','a','n','codex','native','running','2026-11-12T12:00:00.000Z','{\"id\":\"s\",\"threadId\":\"a\",\"runId\":\"run-s\",\"status\":\"running\",\"title\":\"Sub\"}')",
        [],
    ).unwrap();
}

#[test]
fn plan_timestamp_ties_follow_the_proved_run_ordinal_order() {
    let mut db = Db::new();
    seed_plan_threads(&mut db);
    insert_tied_plans(&db);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    // Index order: lowest ordinal, then lowest rowid. The low rowids are the
    // high ordinals, so a rowid tie-break would pick late-* instead.
    for (id, text) in [("z", "win-z"), ("a", "win-a"), ("m", "win-m"), ("s", "win-s")] {
        assert_eq!(plan_text(&first.snapshot, id).as_deref(), Some(text), "{id}");
    }
    db.write("a", 10);
    let tied = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(tied.served, Served::Incremental, "{:?}", tied.counters.fallback_reason);
    assert_eq!(tied.counters.rows_read, tied.counters.cache_rows_read);
    for (id, text) in [("z", "win-z"), ("a", "win-a"), ("m", "win-m"), ("s", "win-s")] {
        assert_eq!(plan_text(&tied.snapshot, id).as_deref(), Some(text), "{id}");
    }
    // Same row, new step text. Same ordinal and a higher rowid still loses.
    db.db.execute(
        "UPDATE orchestration_v2_projection_turn_items SET payload_json=? WHERE turn_item_id='p-z-win'",
        [r#"{"steps":[{"text":"win-z-edited","status":"pending"}]}"#],
    ).unwrap();
    db.write("z", 11);
    let edited = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(edited.served, Served::Incremental, "{:?}", edited.counters.fallback_reason);
    assert_eq!(plan_text(&edited.snapshot, "z").as_deref(), Some("win-z-edited"));
    db.db.execute("DELETE FROM orchestration_v2_projection_turn_items WHERE turn_item_id='p-m-win'", []).unwrap();
    db.write("m", 12);
    let deleted = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(deleted.served, Served::Incremental, "{:?}", deleted.counters.fallback_reason);
    assert_eq!(plan_text(&deleted.snapshot, "m").as_deref(), Some("dup-m"));
    // A later timestamp beats every tie, whatever its ordinal and rowid are.
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_turn_items(rowid,turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json) VALUES(5,'p-z-new','z','run-z',99,'todo_list','completed','2026-11-12T13:00:00.000Z','{\"steps\":[{\"text\":\"newer-z\",\"status\":\"pending\"}]}')",
        [],
    ).unwrap();
    db.write("z", 13);
    let newer = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(newer.served, Served::Incremental, "{:?}", newer.counters.fallback_reason);
    assert_eq!(plan_text(&newer.snapshot, "z").as_deref(), Some("newer-z"));
    // A different index with the same run_id constraint is not this encounter order.
    db.db.execute_batch(
        "DROP INDEX orchestration_v2_projection_turn_items_run_ordinal_idx;
         CREATE INDEX synthetic_plan_order ON orchestration_v2_projection_turn_items(run_id, updated_at DESC)",
    ).unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.write("a", 14);
    let rejected = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(rejected.counters.fallback_reason, Some(FallbackReason::PlanTimestampTie));

    // Full scan: encounter is rowid, so the low-rowid high ordinal wins the tie.
    let mut scanned = Db::new();
    seed_plan_threads(&mut scanned);
    insert_tied_plans(&scanned);
    let mut reader = Reader::open(&scanned.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &scanned, NOW, &mut prev);
    let indexes: Vec<String> = scanned.db.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='orchestration_v2_projection_turn_items' AND sql IS NOT NULL").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    for index in indexes {
        scanned.db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
    }
    assert_eq!(poll(&mut reader, &scanned, NOW, &mut prev).served, Served::Full);
    scanned.write("m", 20);
    let scan = poll(&mut reader, &scanned, NOW, &mut prev);
    assert_eq!(scan.served, Served::Incremental, "{:?}", scan.counters.fallback_reason);
    assert_eq!(scan.counters.rows_read, scan.counters.cache_rows_read);
    for (id, text) in [("z", "late-z"), ("a", "late-a"), ("m", "late-m"), ("s", "late-s")] {
        assert_eq!(plan_text(&scan.snapshot, id).as_deref(), Some(text), "{id}");
    }
}

fn item_labels(snapshot: &Snapshot, id: &str) -> Vec<String> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent.items.iter().map(|item| item.label.clone()).collect())
        .unwrap_or_default()
}

fn seed_item_threads(db: &mut Db) {
    // z, a, then runless m. Insertion order is not thread-id order.
    for (step, owner) in ["z", "a", "m"].iter().enumerate() {
        db.write(owner, step as u64);
    }
    for owner in ["z", "a"] {
        db.db
            .execute(
                "INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json) VALUES(?,?,1,'codex','running','2026-11-12T12:00:00.000Z',?)",
                rusqlite::params![
                    format!("run-{owner}"),
                    owner,
                    format!(r#"{{"id":"run-{owner}","threadId":"{owner}","status":"running"}}"#)
                ],
            )
            .unwrap();
    }
}

/// Tied tool rows whose insertion order, rowid order and index encounter order
/// disagree, both inside the 3,000-row cap and past it.
fn insert_tied_items(db: &Db) {
    let tie = "2026-11-12T12:00:00.000Z";
    let newer = "2026-11-12T13:00:00.000Z";
    let tx = db.db.unchecked_transaction().unwrap();
    let put = |rowid: i64, id: &str, thread: &str, run: Option<&str>, ordinal: i64, stamp: &str, title: &str| {
        tx.execute(
            "INSERT INTO orchestration_v2_projection_turn_items(rowid,turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,?,?,'command_execution','completed',?,?)",
            rusqlite::params![
                rowid,
                id,
                thread,
                run,
                ordinal,
                stamp,
                format!(r#"{{"title":"{title}","input":"synthetic"}}"#)
            ],
        )
        .unwrap();
    };
    // Lowest rowids, inserted first, on the runless thread. Branch 2, so the
    // cap drops them even though a rowid walk would keep them.
    for i in 0..500 {
        put(i + 1, &format!("m-{i}"), "m", None, 0, tie, &format!("m-{i:04}"));
    }
    // run-z before run-a in insertion and rowid, after it in run-id order.
    for i in (0..2000).rev() {
        put(1000 + i, &format!("z-{i}"), "z", Some("run-z"), 0, tie, &format!("z-{i:04}"));
    }
    for i in (0..2000).rev() {
        put(5000 + i, &format!("a-{i}"), "a", Some("run-a"), 0, tie, &format!("a-{i:04}"));
    }
    // Same run, newer stamp, away from the cut. The high ordinal has the high rowid.
    put(8100, "a-hi", "a", Some("run-a"), 5, newer, "hi");
    put(8001, "a-ord9", "a", Some("run-a"), 9, newer, "ord9");
    put(8050, "a-lo", "a", Some("run-a"), 5, newer, "lo");
    put(8000, "a-ord1", "a", Some("run-a"), 1, newer, "ord1");
    tx.commit().unwrap();
}

#[test]
fn item_timestamp_ordinal_ties_follow_the_proved_or_order() {
    let mut db = Db::new();
    seed_item_threads(&mut db);
    insert_tied_items(&db);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    // Touch every owner so the cap is rebuilt from the ordered prefix, not reused.
    db.write("z", 10);
    db.write("a", 11);
    db.write("m", 12);
    let tied = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(tied.served, Served::Incremental, "{:?}", tied.counters.fallback_reason);
    assert_eq!(tied.counters.rows_read, tied.counters.cache_rows_read);
    // Newer stamp, then ordinal DESC, then rowid. A rowid walk would put ord1 first.
    assert_eq!(
        item_labels(&tied.snapshot, "a").iter().take(4).map(String::as_str).collect::<Vec<_>>(),
        ["ord9", "lo", "hi", "ord1"]
    );
    // Branch 1 (run-a, then run-z) fills the cap ahead of runless m, whose
    // rowids are the lowest in the table. run-z keeps its lowest rowids.
    assert_eq!(item_labels(&tied.snapshot, "m").len(), 0);
    let z = item_labels(&tied.snapshot, "z");
    assert_eq!(z.len(), 996);
    assert_eq!(z.first().map(String::as_str), Some("z-0000"));
    assert_eq!(z.last().map(String::as_str), Some("z-0995"));
    assert_eq!(item_labels(&tied.snapshot, "a").len(), 2004);
    // A later row inside the kept prefix stays incremental.
    db.db.execute(
        "UPDATE orchestration_v2_projection_turn_items SET payload_json=? WHERE turn_item_id='a-ord9'",
        [r#"{"title":"ord9-edited","input":"synthetic"}"#],
    ).unwrap();
    db.write("a", 13);
    let edited = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(edited.served, Served::Incremental, "{:?}", edited.counters.fallback_reason);
    assert_eq!(item_labels(&edited.snapshot, "a")[0], "ord9-edited");

    // Replacing the named run index changes the plan. Ties stay a fallback.
    db.db.execute_batch(
        "DROP INDEX orchestration_v2_projection_turn_items_run_ordinal_idx;
         CREATE INDEX synthetic_item_order ON orchestration_v2_projection_turn_items(run_id, updated_at DESC)",
    ).unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.write("z", 12);
    let rejected = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(
        rejected.counters.fallback_reason,
        Some(FallbackReason::ItemTimestampOrdinalTie)
    );

    // No turn-item index: the query is a scan, and equal keys stay in rowid order.
    let mut scanned = Db::new();
    seed_item_threads(&mut scanned);
    insert_tied_items(&scanned);
    let mut reader = Reader::open(&scanned.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &scanned, NOW, &mut prev);
    let indexes: Vec<String> = scanned.db.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='orchestration_v2_projection_turn_items' AND sql IS NOT NULL").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    for index in indexes {
        scanned.db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
    }
    assert_eq!(poll(&mut reader, &scanned, NOW, &mut prev).served, Served::Full);
    scanned.write("z", 20);
    scanned.write("a", 21);
    scanned.write("m", 22);
    let scan = poll(&mut reader, &scanned, NOW, &mut prev);
    assert_eq!(scan.served, Served::Incremental, "{:?}", scan.counters.fallback_reason);
    assert_eq!(scan.counters.rows_read, scan.counters.cache_rows_read);
    assert_eq!(
        item_labels(&scan.snapshot, "a").iter().take(4).map(String::as_str).collect::<Vec<_>>(),
        ["ord9", "lo", "hi", "ord1"]
    );
    assert_eq!(item_labels(&scan.snapshot, "m").len(), 500);
    assert_eq!(item_labels(&scan.snapshot, "z").len(), 2000);
    assert_eq!(item_labels(&scan.snapshot, "a").len(), 500);
}

fn span_starts(snapshot: &Snapshot, id: &str) -> Vec<f64> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent.spans.iter().map(|span| span.0).collect())
        .unwrap_or_default()
}

/// In-window runs whose rowid order is the reverse of `(thread_id, ordinal)`.
/// One run on `a` is outside the window and sits between the others by rowid.
fn insert_span_runs(db: &Db) {
    let rows: [(i64, &str, &str, i64, &str, Option<&str>); 5] = [
        (3, "z-5", "z", 5, "2026-11-12T11:30:00.000Z", None),
        (1, "a-2", "a", 2, "2026-11-12T11:00:00.000Z", Some("2026-11-12T11:30:00.000Z")),
        (2, "a-3", "a", 3, "2026-11-12T08:00:00.000Z", Some("2026-11-12T08:00:00.000Z")),
        (40, "z-4", "z", 4, "2026-11-12T10:30:00.000Z", None),
        (30, "a-1", "a", 1, "2026-11-12T10:00:00.000Z", None),
    ];
    for (rowid, id, thread, ordinal, started, completed) in rows {
        db.db
            .execute(
                "INSERT INTO orchestration_v2_projection_runs(rowid,run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json) VALUES(?,?,?,?,'codex','running',?,?,?)",
                rusqlite::params![
                    rowid,
                    id,
                    thread,
                    ordinal,
                    started,
                    completed,
                    format!(r#"{{"id":"{id}","threadId":"{thread}","status":"running","startedAt":"{started}"}}"#)
                ],
            )
            .unwrap();
    }
}

fn nudge_span_starts(db: &Db) {
    db.db
        .execute(
            "UPDATE orchestration_v2_projection_runs SET payload_json=json_set(payload_json,'$.startedAt','2026-11-12T10:00:01.000Z') WHERE run_id='a-1'",
            [],
        )
        .unwrap();
    db.db
        .execute(
            "UPDATE orchestration_v2_projection_runs SET payload_json=json_set(payload_json,'$.startedAt','2026-11-12T10:30:01.000Z') WHERE run_id='z-4'",
            [],
        )
        .unwrap();
}

#[test]
fn several_spans_follow_the_proved_thread_ordinal_order() {
    let mut db = Db::new();
    db.write("z", 0);
    db.write("a", 1);
    insert_span_runs(&db);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    // A changed run forces the span list to be rebuilt from the proved order.
    nudge_span_starts(&db);
    db.write("a", 2);
    db.write("z", 3);
    let tied = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(tied.served, Served::Incremental, "{:?}", tied.counters.fallback_reason);
    assert_eq!(tied.counters.rows_read, tied.counters.cache_rows_read);
    let a = span_starts(&tied.snapshot, "a");
    let z = span_starts(&tied.snapshot, "z");
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(z.len(), 2, "{z:?}");
    // ordinal 1 then 2, and 4 then 5. Rowid order is the reverse, and it would
    // also have kept a's out-of-window rowid 2 if the filter were ignored.
    assert!(a[0] < a[1], "ordinal order, not rowid: {a:?}");
    assert!(z[0] < z[1], "ordinal order, not rowid: {z:?}");

    db.db.execute_batch(
        "DROP INDEX orchestration_v2_projection_runs_thread_ordinal_idx;
         CREATE INDEX synthetic_span_order ON orchestration_v2_projection_runs(thread_id, completed_at DESC)",
    ).unwrap();
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Full);
    db.write("a", 4);
    let rejected = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(rejected.counters.fallback_reason, Some(FallbackReason::MultipleSpans));

    let mut scanned = Db::new();
    scanned.write("z", 0);
    scanned.write("a", 1);
    insert_span_runs(&scanned);
    let mut reader = Reader::open(&scanned.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &scanned, NOW, &mut prev);
    let indexes: Vec<String> = scanned.db.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='orchestration_v2_projection_runs' AND sql IS NOT NULL").unwrap()
        .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    for index in indexes {
        scanned.db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
    }
    assert_eq!(poll(&mut reader, &scanned, NOW, &mut prev).served, Served::Full);
    nudge_span_starts(&scanned);
    scanned.write("a", 2);
    scanned.write("z", 3);
    let scan = poll(&mut reader, &scanned, NOW, &mut prev);
    assert_eq!(scan.served, Served::Incremental, "{:?}", scan.counters.fallback_reason);
    assert_eq!(scan.counters.rows_read, scan.counters.cache_rows_read);
    let a = span_starts(&scan.snapshot, "a");
    let z = span_starts(&scan.snapshot, "z");
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(z.len(), 2, "{z:?}");
    assert!(a[0] > a[1], "rowid order: {a:?}");
    assert!(z[0] > z[1], "rowid order: {z:?}");
}

#[test]
fn label_only_remote_children_stay_distinct_on_a_graph_poll() {
    use rusqlite::params;
    use serde_json::json;
    let mut db = Db::new();
    db.write("host", 1);
    db.write("other", 2);
    for id in ["s-a", "s-b"] {
        db.db
            .execute(
                "INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES(?,?,'parent','codex','native','running','2026-11-12T12:00:00.000Z',?)",
                params![
                    id,
                    "host",
                    json!({
                        "id": id,
                        "threadId": "host",
                        "parentNodeId": "parent",
                        "status": "running",
                        "title": id,
                        "remoteChild": {"label": "sandbox"}
                    })
                    .to_string()
                ],
            )
            .unwrap();
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    let remote = |id: &str, snapshot: &Snapshot| {
        snapshot
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .unwrap()
            .remote
            .clone()
    };
    assert_eq!(remote("s-a", &first.snapshot), remote("s-b", &first.snapshot));
    assert!(remote("s-a", &first.snapshot).is_some());
    db.db
        .execute(
            "INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('s-c','host','parent','codex','native','running','2026-11-12T12:00:01.000Z',?)",
            params![json!({
                "id": "s-c",
                "threadId": "host",
                "parentNodeId": "parent",
                "status": "running",
                "title": "s-c",
                "remoteChild": {"label": "sandbox"}
            })
            .to_string()],
        )
        .unwrap();
    db.write("host", 3);
    let next = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(next.served, Served::Incremental);
    assert!(next.snapshot.agents.iter().any(|agent| agent.id == "s-c"));
    assert_eq!(remote("s-a", &next.snapshot), remote("s-c", &next.snapshot));
    assert_eq!(reader.mismatches(), 0);
}

fn item_stamp(i: i64) -> String {
    format!("2026-11-12T12:00:{:02}.{:03}Z", i / 1000, i % 1000)
}

fn insert_command(
    db: &rusqlite::Connection,
    id: &str,
    owner: &str,
    ordinal: i64,
    stamp: &str,
    command: &str,
) {
    db.execute(
        "INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,?,?,'command_execution','running',?,?)",
        rusqlite::params![id, owner, ordinal, stamp, format!(r#"{{"input":"{command}"}}"#)],
    )
    .unwrap();
}

fn commands_of(snapshot: &Snapshot, agent: &str) -> Vec<String> {
    snapshot
        .agents
        .iter()
        .find(|found| found.id == agent)
        .unwrap()
        .items
        .iter()
        .map(|item| item.command.clone())
        .collect()
}

/// 3,000 in-window commands on `a`, plus one on `b` dated 2020. The old row is
/// outside the item cap and outside the event window, so its command is not cached.
fn fill_cap_with_one_outside(db: &mut Db) {
    db.write("a", 0);
    db.write("b", 1);
    let tx = db.db.transaction().unwrap();
    for i in 1..=3000 {
        insert_command(&tx, &format!("a{i}"), "a", i, &item_stamp(i), &format!("hot-{i}"));
    }
    insert_command(&tx, "cold", "b", 0, "2020-01-01T00:00:00.000Z", "cold-command");
    tx.commit().unwrap();
    db.write("a", 2);
    db.write("b", 3);
}

fn demote(db: &Db, id: &str) {
    db.db
        .execute(
            "UPDATE orchestration_v2_projection_turn_items SET updated_at='2019-01-01T00:00:00.000Z' WHERE turn_item_id=?",
            [id],
        )
        .unwrap();
}

#[test]
fn entering_cap_reads_text_after_an_in_cap_row_leaves() {
    let mut db = Db::new();
    fill_cap_with_one_outside(&mut db);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(first.counters.fallback_reason, Some(FallbackReason::InitialBaseline));
    assert!(!commands_of(&first.snapshot, "b").iter().any(|c| c == "cold-command"));
    // a3000 was the newest in-cap row. Dating it before the cold row lets that
    // row into the item cap. It was never an event, so the command has to be read.
    demote(&db, "a3000");
    db.write("a", 4);
    let entered = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(entered.served, Served::Incremental, "{:?}", entered.counters.fallback_reason);
    assert_eq!(entered.counters.rows_read, entered.counters.cache_rows_read);
    assert!(commands_of(&entered.snapshot, "b").iter().any(|c| c == "cold-command"));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn row_leaves_the_cap_and_reentering_reads_its_text_again() {
    let mut db = Db::new();
    fill_cap_with_one_outside(&mut db);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    demote(&db, "a3000");
    db.write("a", 4);
    let entered = poll(&mut reader, &db, NOW, &mut prev);
    assert!(commands_of(&entered.snapshot, "b").iter().any(|c| c == "cold-command"));
    insert_command(&db.db, "pusher", "a", 4000, &item_stamp(4000), "pusher");
    db.write("a", 5);
    let pushed = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(pushed.served, Served::Incremental);
    assert!(!commands_of(&pushed.snapshot, "b").iter().any(|c| c == "cold-command"));
    // The command changes while the row is outside every selection, and b is
    // not in this poll's changed set. A kept copy would still say "cold-command".
    db.db
        .execute(
            "UPDATE orchestration_v2_projection_turn_items SET payload_json=? WHERE turn_item_id='cold'",
            [r#"{"input":"cold-again"}"#],
        )
        .unwrap();
    demote(&db, "pusher");
    db.write("a", 6);
    let again = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(again.served, Served::Incremental, "{:?}", again.counters.fallback_reason);
    assert_eq!(again.counters.rows_read, again.counters.cache_rows_read);
    assert!(commands_of(&again.snapshot, "b").iter().any(|c| c == "cold-again"));
    assert!(!commands_of(&again.snapshot, "b").iter().any(|c| c == "cold-command"));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn burst_turns_over_many_cap_members_and_reads_their_text() {
    let mut db = Db::new();
    db.write("a", 0);
    let tx = db.db.transaction().unwrap();
    for i in 0..8300 {
        insert_command(&tx, &format!("i{i}"), "a", i, &item_stamp(i), &format!("row-{i}"));
    }
    tx.commit().unwrap();
    db.write("a", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    // The oldest 300 sit outside the 8,000-event cap and the 3,000-item cap.
    assert!(!commands_of(&first.snapshot, "a").iter().any(|c| c == "row-0"));
    assert!(!commands_of(&first.snapshot, "a").iter().any(|c| c == "row-299"));
    let tx = db.db.transaction().unwrap();
    for i in 3000..8300 {
        tx.execute(
            "UPDATE orchestration_v2_projection_turn_items SET updated_at='2019-01-01T00:00:00.000Z' WHERE turn_item_id=?",
            [format!("i{i}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    db.write("a", 2);
    let turned = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(turned.served, Served::Incremental, "{:?}", turned.counters.fallback_reason);
    assert_eq!(turned.counters.rows_read, turned.counters.cache_rows_read);
    let commands = commands_of(&turned.snapshot, "a");
    assert!(commands.iter().any(|c| c == "row-0"));
    assert!(commands.iter().any(|c| c == "row-299"));
    assert!(!commands.iter().any(|c| c == "row-8299"));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn clock_expiry_then_entry_reads_text_dropped_with_the_window() {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    let tx = db.db.transaction().unwrap();
    for i in 1..=3000 {
        insert_command(
            &tx,
            &format!("a{i}"),
            "a",
            i,
            &format!("2026-11-12T12:00:{:02}.{:03}Z", (i / 1000) + 1, i % 1000),
            &format!("hot-{i}"),
        );
    }
    // Inside the event window, outside the item cap. The baseline caches its text
    // for the event. A forward clock cannot pull a newer outside row in; it only
    // drops this one. A later write on `a` is what admits it to the item cap.
    insert_command(&tx, "cold", "b", 0, "2026-11-12T11:00:00.000Z", "cold-command");
    tx.commit().unwrap();
    db.write("a", 2);
    db.write("b", 3);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert!(!commands_of(&first.snapshot, "b").iter().any(|c| c == "cold-command"));
    assert!(!first.snapshot.agents.iter().find(|a| a.id == "b").unwrap().events.is_empty());
    // 11:00:00.000 falls out at 14:00:00.001; the noon rows stay.
    let expired = poll(&mut reader, &db, NOW + 7_200_001., &mut prev);
    assert_eq!(expired.served, Served::Incremental, "{:?}", expired.counters.fallback_reason);
    assert_eq!(expired.counters.rows_read, expired.counters.cache_rows_read);
    assert!(expired.snapshot.agents.iter().find(|a| a.id == "b").unwrap().events.is_empty());
    assert!(!commands_of(&expired.snapshot, "b").iter().any(|c| c == "cold-command"));
    demote(&db, "a3000");
    db.write("a", 4);
    let entered = poll(&mut reader, &db, NOW + 7_200_001., &mut prev);
    assert_eq!(entered.served, Served::Incremental, "{:?}", entered.counters.fallback_reason);
    assert_eq!(entered.counters.rows_read, entered.counters.cache_rows_read);
    assert!(commands_of(&entered.snapshot, "b").iter().any(|c| c == "cold-command"));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn text_drift_falls_back_once_and_the_next_poll_is_incremental() {
    let mut db = Db::new();
    db.write("a", 0);
    insert_command(&db.db, "i", "a", 1, &item_stamp(1), "synthetic");
    db.write("a", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(first.counters.fallback_reason, Some(FallbackReason::InitialBaseline));
    assert!(commands_of(&first.snapshot, "a").iter().any(|c| c == "synthetic"));
    reader.force_text_drift();
    db.write("a", 2);
    let drifted = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(drifted.served, Served::Full);
    assert_eq!(drifted.counters.fallback_reason, Some(FallbackReason::TextDrift));
    assert!(commands_of(&drifted.snapshot, "a").iter().any(|c| c == "synthetic"));
    db.write("a", 3);
    let next = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
    assert_eq!(next.counters.rows_read, next.counters.cache_rows_read);
    assert!(commands_of(&next.snapshot, "a").iter().any(|c| c == "synthetic"));
    assert_eq!(reader.mismatches(), 0);
}

// A real installation appends to `orchestration_events` and leaves `orchestration_v2_events`
// empty. The cases below drive the reader through that log.

#[test]
fn the_real_event_log_serves_thread_changes_incrementally() {
    let mut db = Db::new();
    db.write_application("a", 0);
    db.write_application("b", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let initial = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(initial.served, Served::Full);
    assert_eq!(initial.counters.fallback_reason, Some(FallbackReason::InitialBaseline));
    for step in 1..6 {
        db.write_application("a", step);
        let next = poll(&mut reader, &db, NOW, &mut previous);
        assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
        assert!(next.counters.feed_complete);
        assert_eq!(next.counters.changed_threads, 1);
    }
    db.write_application("a", 6);
    db.write_application("b", 1);
    db.write_application("c", 0);
    let burst = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(burst.served, Served::Incremental, "{:?}", burst.counters.fallback_reason);
    assert_eq!(burst.counters.changed_threads, 3);
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn an_event_that_names_no_thread_forces_one_full_read() {
    // A version-2 project event changes `projection_projects` alone, which every changed
    // poll reads in full; with a ledger to count that is served from the cache. Without
    // one, and for anything that is neither a version-2 thread nor a project event, the
    // poll reads in full.
    for (kind, stream, version, ledger, full) in [
        ("project", "p", 2, false, true),
        ("project", "p", 2, true, false),
        ("thread", "a", 1, false, true),
        ("thread", "a", 1, true, true),
        ("workspace", "w", 2, false, true),
        ("workspace", "w", 2, true, true),
        ("project", "p", 1, true, true),
    ] {
        let mut db = if ledger { Db::new() } else { Db::without_ledger() };
        db.write_application("a", 0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut previous = None;
        poll(&mut reader, &db, NOW, &mut previous);
        db.write_application("a", 1);
        assert_eq!(poll(&mut reader, &db, NOW, &mut previous).served, Served::Incremental);
        db.log_application("other", kind, stream, version);
        db.db
            .execute("UPDATE projection_projects SET title='Renamed'", [])
            .unwrap();
        let other = poll(&mut reader, &db, NOW, &mut previous);
        let case = format!("{kind} v{version}, ledger {ledger}");
        if full {
            assert_eq!(other.served, Served::Full, "{case}");
            assert_eq!(other.counters.fallback_reason, Some(FallbackReason::NonThreadEvent), "{case}");
        } else {
            assert_eq!(other.served, Served::Incremental, "{case}: {:?}", other.counters.fallback_reason);
            assert!(other.counters.feed_complete, "{case}");
            assert_eq!(other.counters.changed_threads, 0, "{case}");
        }
        // The next thread event is incremental again.
        db.write_application("a", 2);
        let next = poll(&mut reader, &db, NOW, &mut previous);
        assert_eq!(next.served, Served::Incremental, "{case}: {:?}", next.counters.fallback_reason);
        assert_eq!(reader.mismatches(), 0);
    }
}

#[test]
fn a_cursor_from_one_log_is_never_applied_to_the_other() {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("a", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    poll(&mut reader, &db, NOW, &mut previous);
    db.write("a", 2);
    assert_eq!(poll(&mut reader, &db, NOW, &mut previous).served, Served::Incremental);
    // The projection now follows the real log, whose numbering starts over.
    for step in 0..4 {
        db.write_application("b", step);
    }
    let switched = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(switched.served, Served::Full);
    assert_eq!(switched.counters.fallback_reason, Some(FallbackReason::FeedBaseline));
    db.write_application("b", 4);
    let next = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn pruning_the_real_log_below_the_cursor_keeps_polls_incremental() {
    let mut db = Db::new();
    for step in 0..8 {
        db.write_application("a", step);
    }
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    poll(&mut reader, &db, NOW, &mut previous);
    db.write_application("a", 8);
    assert_eq!(poll(&mut reader, &db, NOW, &mut previous).served, Served::Incremental);
    // T3 deletes old events without touching its marks. Rows far below the cursor are
    // nothing the reader still needs.
    db.db
        .execute("DELETE FROM orchestration_events WHERE sequence < 5", [])
        .unwrap();
    db.write_application("a", 9);
    let next = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
    // Moving a prune mark is a change the reader does not interpret: one full read.
    db.db
        .execute("INSERT INTO orchestration_v2_projection_metadata VALUES('event-log-prune-mark',2,4,'')", [])
        .unwrap();
    db.write_application("a", 10);
    let marked = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(marked.counters.fallback_reason, Some(FallbackReason::PruningMarkerChanged));
    db.write_application("a", 11);
    let next = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
    assert_eq!(reader.mismatches(), 0);
}

fn parent_provider_mutation(mutation: &str) {
    for check in [false, true] {
        let mut db = Db::new();
        support::parent_provider_child(&mut db);
        if mutation.starts_with("UPDATE orchestration_v2_projection_runs") {
            db.db.execute_batch(r#"INSERT INTO orchestration_v2_projection_turn_items
                (turn_item_id,thread_id,run_id,provider_thread_id,ordinal,type,status,updated_at,payload_json)
                VALUES('foreign-item','child','r','pt',1,'command_execution','running',
                '2026-11-12T12:00:00.000Z','{"input":"synthetic","status":"running"}')"#).unwrap();
        }
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(check);
        let mut prev = None;
        let first = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(first.snapshot.agents.iter().find(|a| a.id == "child").unwrap().history.len(), 1);
        db.db.execute_batch(mutation).unwrap();
        db.write("parent", 2);
        // The parent's rows feed the child's agent. The poll is served from the
        // cache and `poll` holds it to the oracle, the Arcs and `Changed`.
        let changed = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(changed.served, Served::Incremental, "{mutation}");
        assert_eq!(changed.counters.fallback_reason, None, "{mutation}");
        let child = changed.snapshot.agents.iter().find(|a| a.id == "child");
        if mutation.starts_with("INSERT INTO orchestration_v2_projection_provider_turns") {
            // The parent-owned turn routes through the alias to the child.
            assert_eq!(child.unwrap().history.len(), 2, "{mutation}");
        }
        poll(&mut reader, &db, NOW + 10_800_001., &mut prev);
        if mutation.starts_with("DELETE FROM orchestration_v2_projection_subagents") {
            db.write("parent", 3);
            let local = poll(&mut reader, &db, NOW + 10_800_001., &mut prev);
            assert_eq!(local.served, Served::Incremental);
        }
        assert_eq!(reader.mismatches(), 0);
    }
}

#[test]
fn cross_thread_parent_provider_turn() {
    parent_provider_mutation(r#"INSERT INTO orchestration_v2_projection_provider_turns
        (provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json)
        VALUES('turn2','parent','pt','n',2,'running','2026-11-12T11:01:00.000Z',
        '{"providerThreadId":"pt","model":"synthetic","tokenUsage":{"usedTokens":2,"maxTokens":100}}')"#);
}
#[test]
fn cross_thread_complete_run() {
    parent_provider_mutation(r#"UPDATE orchestration_v2_projection_runs SET status='completed',
        completed_at='2026-11-12T11:02:00.000Z',
        payload_json=json_set(payload_json,'$.status','completed') WHERE run_id='r';
        UPDATE orchestration_v2_projection_turn_items SET status='completed',
        payload_json=json_set(payload_json,'$.status','completed') WHERE run_id='r'"#);
}
#[test]
fn cross_thread_archive_thread() {
    parent_provider_mutation("UPDATE orchestration_v2_projection_threads SET archived_at='synthetic' WHERE thread_id='parent'");
}
#[test]
fn cross_thread_delete_thread() {
    parent_provider_mutation("UPDATE orchestration_v2_projection_threads SET deleted_at='synthetic' WHERE thread_id='parent'");
}
#[test]
fn cross_thread_removed_alias_then_local_graph() {
    parent_provider_mutation("DELETE FROM orchestration_v2_projection_subagents");
}

#[test]
fn cross_thread_node_run_and_provider_references_are_served_from_the_cache() {
    for reference in [
        // Provider consumers may be thread shells, run shells or pure subagents.
        r#"UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.activeProviderThreadId','pt') WHERE thread_id='child'"#,
        r#"INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
            VALUES('rc','child',1,'codex','running','','{"id":"rc","threadId":"child","providerThreadId":"pt"}')"#,
        r#"INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json)
            VALUES('sc','child','n','codex','native','running','','{"id":"sc","threadId":"child","providerThreadId":"pt"}')"#,
        // A node chain, request, event/item/plan and run selection can all cross owners.
        r#"INSERT INTO orchestration_v2_projection_nodes(node_id,thread_id,parent_node_id,root_node_id,kind,status,payload_json)
            VALUES('nc','child','s','root','tool','running','{}')"#,
        r#"INSERT INTO orchestration_v2_projection_runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,payload_json)
            VALUES('q','child','s','approval','open','','{}')"#,
        r#"INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
            VALUES('i','child','r','s',1,'command_execution','failed','2026-11-12T12:00:00.000Z','{"input":"synthetic"}')"#,
        r#"INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
            VALUES('plan','child','r','s',1,'todo_list','running','2026-11-12T12:00:00.000Z','{"steps":[]}')"#,
    ] {
        let mut db = Db::new();
        support::parent_provider_child(&mut db);
        // Keep a pure, parent-local subagent. The tested foreign reference,
        // rather than the child alias from the other test, must cause refusal.
        db.db.execute_batch("UPDATE orchestration_v2_projection_subagents SET child_thread_id=NULL,payload_json=json_remove(payload_json,'$.childThreadId')").unwrap();
        db.db.execute_batch(reference).unwrap();
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        poll(&mut reader, &db, NOW, &mut prev);
        db.write("parent", 2);
        let changed = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(changed.served, Served::Incremental, "{reference}");
        assert_eq!(changed.counters.fallback_reason, None, "{reference}");
        // A write on the foreign owner itself reaches the parent's agents too.
        db.write("child", 3);
        let foreign = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(foreign.served, Served::Incremental, "{reference}");
        assert_eq!(reader.mismatches(), 0);
    }
}

#[test]
fn unchanged_request_follows_a_changed_node_parent() {
    let mut db = Db::new();
    support::parent_provider_child(&mut db);
    db.db.execute_batch(r#"
        UPDATE orchestration_v2_projection_subagents SET child_thread_id=NULL,
          payload_json=json_remove(payload_json,'$.childThreadId');
        INSERT INTO orchestration_v2_projection_subagents
          (subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json)
          VALUES('other','parent','root','codex','native','running','',
          '{"id":"other","threadId":"parent","status":"running"}');
        INSERT INTO orchestration_v2_projection_nodes
          (node_id,thread_id,parent_node_id,root_node_id,kind,status,payload_json)
          VALUES('request-node','parent','s','root','tool','running','{}');
        INSERT INTO orchestration_v2_projection_runtime_requests
          (runtime_request_id,thread_id,node_id,kind,status,created_at,payload_json)
          VALUES('q','parent','request-node','approval','open','','{}');
    "#).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.db.execute_batch("UPDATE orchestration_v2_projection_nodes SET parent_node_id='other' WHERE node_id='request-node'").unwrap();
    db.write("parent", 2);
    let changed = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(changed.served, Served::Incremental);
    assert!(!changed.snapshot.agents.iter().find(|a| a.id == "other").unwrap().requests.is_empty());
}

#[test]
fn subagent_without_payload_owner_is_served_from_the_cache() {
    let mut db = Db::new();
    support::parent_provider_child(&mut db);
    db.db.execute_batch("UPDATE orchestration_v2_projection_subagents SET child_thread_id=NULL,payload_json=json_remove(payload_json,'$.childThreadId','$.threadId')").unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.write("parent", 2);
    let changed = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(changed.served, Served::Incremental);
    assert_eq!(changed.counters.fallback_reason, None);
    assert_eq!(reader.mismatches(), 0);
}

// ── Cross-thread links served from the cache ────────────────────────────────
// Every poll below goes through `poll`, which holds the snapshot to the
// oracle, the Arcs to content equality, and `Changed` to the exact difference.

fn agent<'a>(snapshot: &'a Snapshot, id: &str) -> &'a t3_agents_core::model::Agent {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .unwrap_or_else(|| panic!("no agent {id}"))
}
fn changed_ids(update: &t3_agents_core::live::Update) -> Vec<String> {
    match &update.changed {
        Changed::Agents(ids) => ids.clone(),
        Changed::Everything => panic!("expected precise changes"),
    }
}
fn incremental(update: &t3_agents_core::live::Update, what: &str) {
    assert_eq!(update.served, Served::Incremental, "{what}: {:?}", update.counters.fallback_reason);
    assert_eq!(update.counters.fallback_reason, None, "{what}");
}

/// The shapes `cross-machine-parity.ts` builds: local and remote delegation,
/// retries of one remote child, a shared reference-only parent, invalid refs,
/// and a delegation cycle. Each write is served from the cache.
#[test]
fn cross_machine_links_follow_every_write() {
    use serde_json::json;
    for check in [false, true] {
        let mut db = Db::new();
        db.environment("machine-a");
        let refer = |env: &str, thread: &str| json!({"environmentId": env, "threadId": thread});
        db.thread("root", 0, json!({}));
        db.thread("local", 0, json!({"delegatedFrom": refer("machine-a", "root")}));
        db.thread("leaf", 0, json!({"delegatedFrom": refer("machine-b", "parent")}));
        db.thread("unknown", 0, json!({"delegatedFrom": {"environmentId": "machine-c", "threadId": "absent", "title": "Missing remote"}}));
        db.thread("unknown-2", 0, json!({"delegatedFrom": refer("machine-c", "absent")}));
        db.thread("bad", 0, json!({"delegatedFrom": refer("../bad", "parent")}));
        db.thread("incomplete", 0, json!({"delegatedFrom": {"environmentId": "machine-b"}}));
        db.thread("a", 0, json!({"delegatedFrom": refer("machine-a", "b")}));
        db.thread("b", 0, json!({"delegatedFrom": refer("machine-a", "a")}));
        db.db.execute_batch(r#"
            INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
              VALUES('rr','root',1,'codex','running','2026-11-12T10:00:00.000Z','{"id":"rr","threadId":"root","status":"running","startedAt":"2026-11-12T10:00:00.000Z"}');
        "#).unwrap();
        for i in 0..3 {
            let id = format!("retry-{i}");
            let pid = format!("provider-{i}");
            let sub = json!({"id": id, "threadId": "root", "runId": "rr", "providerThreadId": pid,
                "status": if i == 2 {"completed"} else {"running"},
                "updatedAt": format!("2026-11-12T11:00:0{i}.000Z"),
                "remoteChild": {"environmentId": "machine-b", "threadId": "parent", "label": "B", "result": "SECRET"}});
            db.db.execute("INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,origin,status,updated_at,payload_json) VALUES(?,'root','rr','n-root','codex',?,'app_owned','running','',?)",
                rusqlite::params![id, pid, sub.to_string()]).unwrap();
            db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,node_id,provider_thread_id,ordinal,type,status,updated_at,payload_json) VALUES(?,'root','rr',?,?,?,'command_execution','completed','2026-11-12T11:30:00.000Z','{\"input\":\"synthetic command\",\"title\":\"Synthetic\"}')",
                rusqlite::params![format!("item-{i}"), id, pid, i]).unwrap();
            db.db.execute("INSERT INTO orchestration_v2_projection_provider_turns(provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json) VALUES(?,'root',?,'n-root',?,'completed','2026-11-12T11:20:00.000Z',?)",
                rusqlite::params![format!("turn-{i}"), pid, i, json!({"providerThreadId": pid, "tokenUsage": {"usedTokens": 10 + i, "maxTokens": 100}}).to_string()]).unwrap();
        }
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(check);
        let mut prev = None;
        let first = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(agent(&first.snapshot, "leaf").parent.as_deref(), Some("retry-2"));
        assert_eq!(agent(&first.snapshot, "retry-2").subagent_ids.as_ref().unwrap(), &["retry-0", "retry-1", "retry-2"]);
        let reference = first.snapshot.agents.iter().find(|a| a.reference_only == Some(true)).unwrap().id.clone();
        assert_eq!(agent(&first.snapshot, "unknown").parent.as_deref(), Some(reference.as_str()));
        assert!(agent(&first.snapshot, "a").parent.is_none(), "the cycle is cut at the smaller id");

        // An ordinary write on a delegated-in thread touches that agent alone.
        db.touch("leaf", 1, "");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "delegated activity");
        assert_eq!(changed_ids(&next), ["leaf"]);

        // A retry becomes the newest: the shared remote agent takes its id, and
        // the thread delegated from that remote parent follows it.
        db.touch("root", 2, r#"UPDATE orchestration_v2_projection_subagents SET payload_json=json_set(payload_json,'$.updatedAt','2026-11-12T11:00:09.000Z') WHERE subagent_id='retry-0'"#);
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "retry order");
        assert!(next.snapshot.agents.iter().all(|a| a.id != "retry-2"));
        assert_eq!(agent(&next.snapshot, "leaf").parent.as_deref(), Some("retry-0"));

        // Provider history and node-routed items of the root land on the retry agent.
        db.touch("root", 3, r#"INSERT INTO orchestration_v2_projection_provider_turns(provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json) VALUES('turn-x','root','provider-1','n-root',10,'running','2026-11-12T11:40:00.000Z','{"providerThreadId":"provider-1","tokenUsage":{"usedTokens":50,"maxTokens":100}}')"#);
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "provider turn");
        assert_eq!(agent(&next.snapshot, "retry-0").history.len(), 4);
        db.touch("root", 4, "UPDATE orchestration_v2_projection_turn_items SET status='failed',updated_at='2026-11-12T11:45:00.000Z' WHERE node_id='retry-1'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "routed item");
        assert_eq!(agent(&next.snapshot, "retry-0").failures, 1);

        // The reference-only parent survives on its other delegate.
        db.touch("unknown", 5, "UPDATE orchestration_v2_projection_threads SET archived_at='2026-11-12T11:50:00.000Z' WHERE thread_id='unknown'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "archive a delegate");
        let reference = next.snapshot.agents.iter().find(|a| a.reference_only == Some(true)).unwrap();
        assert_eq!(agent(&next.snapshot, "unknown-2").parent.as_deref(), Some(reference.id.as_str()));
        assert_eq!(reference.title, "Remote parent");

        // Breaking the delegation cycle.
        db.touch("b", 6, "UPDATE orchestration_v2_projection_threads SET payload_json=json_remove(payload_json,'$.delegatedFrom') WHERE thread_id='b'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "cycle removed");
        assert_eq!(agent(&next.snapshot, "a").parent.as_deref(), Some("b"));
        assert!(agent(&next.snapshot, "b").parent.is_none());

        // With every retry gone the leaf's parent is only a reference.
        db.touch("root", 7, "DELETE FROM orchestration_v2_projection_subagents");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "retries removed");
        let parent = agent(&next.snapshot, "leaf").parent.clone().unwrap();
        assert_eq!(agent(&next.snapshot, &parent).reference_only, Some(true));

        // A remote child naming the absent thread replaces that reference.
        db.touch("root", 8, r#"INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,run_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('late','root','rr','n-root','codex','app_owned','running','','{"id":"late","threadId":"root","runId":"rr","status":"running","remoteChild":{"environmentId":"machine-c","threadId":"absent","label":"C"}}')"#);
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "remote child for a referenced parent");
        assert_eq!(agent(&next.snapshot, "unknown-2").parent.as_deref(), Some("late"));
        assert_eq!(reader.mismatches(), 0);
    }
}

/// Local child threads: rows owned by one thread reach another thread's agent
/// through a subagent alias, a node chain and a provider id. Links are added,
/// moved and removed, and a retry names a child again.
#[test]
fn families_merge_split_and_reroute_untouched_rows() {
    use serde_json::json;
    for check in [false, true] {
        let mut db = Db::new();
        db.thread("host", 0, json!({}));
        db.thread("child-a", 0, json!({"lineage": {"relationshipToParent": "subagent", "parentThreadId": "host"}, "activeProviderThreadId": "pa"}));
        db.thread("child-b", 0, json!({"lineage": {"relationshipToParent": "subagent", "parentThreadId": "host"}, "activeProviderThreadId": "pb"}));
        db.thread("other", 0, json!({}));
        db.db.execute_batch(r#"
            INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json) VALUES
              ('r-host','host',1,'codex','running','2026-11-12T10:00:00.000Z','{"id":"r-host","threadId":"host","status":"running"}'),
              ('r-a','child-a',1,'codex','running','2026-11-12T10:00:00.000Z','{"id":"r-a","threadId":"child-a","status":"running","providerThreadId":"pa"}'),
              ('r-b','child-b',1,'codex','running','2026-11-12T10:00:00.000Z','{"id":"r-b","threadId":"child-b","status":"running","providerThreadId":"pb"}');
            INSERT INTO orchestration_v2_projection_provider_threads(provider_thread_id,thread_id,provider,status,updated_at,payload_json) VALUES
              ('pa','child-a','codex','active','','{"id":"pa","contextUsage":{"usedTokens":5,"maxTokens":100}}'),
              ('pb','child-b','codex','active','','{"id":"pb","contextUsage":{"usedTokens":6,"maxTokens":100}}');
            INSERT INTO orchestration_v2_projection_provider_turns(provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json) VALUES
              ('ta','child-a','pa','n',1,'completed','2026-11-12T11:00:00.000Z','{"providerThreadId":"pa","model":"m","tokenUsage":{"usedTokens":7,"maxTokens":100}}'),
              ('tb','child-b','pb','n',1,'completed','2026-11-12T11:00:00.000Z','{"providerThreadId":"pb","model":"m","tokenUsage":{"usedTokens":8,"maxTokens":100}}');
            INSERT INTO orchestration_v2_projection_nodes(node_id,thread_id,parent_node_id,root_node_id,kind,status,payload_json) VALUES
              ('n-host','host',NULL,'n-host','root','running','{}'),
              ('n-s-a','host','s-a','n-host','subagent','running','{}'),
              ('n-s-b','host','s-b','n-host','subagent','running','{}');
            INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,child_thread_id,origin,status,updated_at,payload_json) VALUES
              ('s-a','host','r-host','n-host','codex','pa','child-a','app_owned','running','','{"id":"s-a","threadId":"host","runId":"r-host","parentNodeId":"n-host","providerThreadId":"pa","childThreadId":"child-a","status":"running","updatedAt":"2026-11-12T11:00:00.000Z"}'),
              ('s-b','host','r-host','n-host','codex','pb','child-b','app_owned','running','','{"id":"s-b","threadId":"host","runId":"r-host","parentNodeId":"n-host","providerThreadId":"pb","childThreadId":"child-b","status":"running","updatedAt":"2026-11-12T11:00:01.000Z"}');
            INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,node_id,provider_thread_id,ordinal,type,status,updated_at,payload_json) VALUES
              ('host-under-a','host','r-a','n-s-a',NULL,1,'command_execution','running','2026-11-12T11:10:00.000Z','{"input":"under a","startedAt":"2026-11-12T11:09:00.000Z"}'),
              ('other-on-pa','other','r-a',NULL,'pa',2,'command_execution','running','2026-11-12T11:11:00.000Z','{"input":"other on pa"}'),
              ('a-own','child-a','r-a',NULL,'pa',3,'command_execution','completed','2026-11-12T11:12:00.000Z','{"input":"own"}');
        "#).unwrap();
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(check);
        let mut prev = None;
        let first = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(agent(&first.snapshot, "child-a").items.len(), 3, "host and other rows reach child-a");

        // A row owned by `other` reaches child-a through its provider id.
        db.touch("other", 1, "UPDATE orchestration_v2_projection_turn_items SET status='completed',payload_json=json_set(payload_json,'$.input','other done') WHERE turn_item_id='other-on-pa'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "provider-routed write");
        assert!(changed_ids(&next).contains(&"child-a".to_string()));

        // A payload-only edit keeps every key; the agent that shows the row is repainted.
        db.touch("other", 2, "UPDATE orchestration_v2_projection_turn_items SET payload_json=json_set(payload_json,'$.input','renamed') WHERE turn_item_id='other-on-pa'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "payload-only edit");
        assert!(agent(&next.snapshot, "child-a").items.iter().any(|item| item.command == "renamed"));

        // Moving a node moves the host's row from child-a to child-b. Neither
        // child's own rows changed.
        db.touch("host", 3, "UPDATE orchestration_v2_projection_nodes SET parent_node_id='s-b' WHERE node_id='n-s-a'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "node moved");
        let ids = changed_ids(&next);
        assert!(ids.contains(&"child-a".to_string()) && ids.contains(&"child-b".to_string()), "{ids:?}");

        // Removing child-b's link splits it off: the host's row comes home.
        db.touch("host", 4, "DELETE FROM orchestration_v2_projection_subagents WHERE subagent_id='s-b'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "link removed");
        assert_eq!(agent(&next.snapshot, "child-b").kind, "thread");
        assert!(agent(&next.snapshot, "host").events.len() >= 1);

        // A lineage change on the unlinked child is its own structural write.
        db.touch("child-b", 5, "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.lineage.parentThreadId','other') WHERE thread_id='child-b'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "lineage parent");
        assert_eq!(agent(&next.snapshot, "child-b").parent.as_deref(), Some("other"));

        // The link comes back, and a retry names child-a again from under child-b.
        db.touch("host", 6, r#"
            INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,child_thread_id,origin,status,updated_at,payload_json) VALUES
              ('s-b','host','r-host','n-host','codex','pb','child-b','app_owned','running','','{"id":"s-b","threadId":"host","runId":"r-host","parentNodeId":"n-host","providerThreadId":"pb","childThreadId":"child-b","status":"running","updatedAt":"2026-11-12T11:00:01.000Z"}'),
              ('s-a2','host','r-host','n-s-b','codex','pa','child-a','app_owned','running','','{"id":"s-a2","threadId":"host","runId":"r-host","parentNodeId":"n-s-b","providerThreadId":"pa","childThreadId":"child-a","status":"running","updatedAt":"2026-11-12T11:30:00.000Z"}');
        "#);
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "retry");
        assert_eq!(agent(&next.snapshot, "child-a").subagent_ids.as_ref().unwrap(), &["s-a", "s-a2"]);
        assert_eq!(agent(&next.snapshot, "child-a").parent.as_deref(), Some("child-b"));

        // Ordinary writes on each member of the merged family stay incremental
        // and reach no other agent.
        for (step, id) in [(7, "child-a"), (8, "child-b"), (9, "host"), (10, "other")] {
            db.touch(id, step, "");
            let next = poll(&mut reader, &db, NOW, &mut prev);
            incremental(&next, id);
            assert!(changed_ids(&next).iter().all(|changed| changed == id), "{id}: {:?}", changed_ids(&next));
        }
        assert_eq!(reader.mismatches(), 0);
    }
}

/// Two child threads that each name the other through subagent rows and
/// nodes. The oracle cuts the cycle at the smaller id; so does the cache.
#[test]
fn a_subagent_cycle_is_cut_like_the_oracle() {
    use serde_json::json;
    let mut db = Db::new();
    db.thread("x", 0, json!({}));
    db.thread("y", 0, json!({}));
    db.db.execute_batch(r#"
        INSERT INTO orchestration_v2_projection_nodes(node_id,thread_id,parent_node_id,root_node_id,kind,status,payload_json) VALUES
          ('n-x','x','sx','n-x','subagent','running','{}'),
          ('n-y','y','sy','n-y','subagent','running','{}');
        INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,child_thread_id,origin,status,updated_at,payload_json) VALUES
          ('sx','x','n-y','codex','y','app_owned','running','','{"id":"sx","threadId":"x","parentNodeId":"n-y","childThreadId":"y","status":"running"}'),
          ('sy','y','n-x','codex','x','app_owned','running','','{"id":"sy","threadId":"y","parentNodeId":"n-x","childThreadId":"x","status":"running"}');
    "#).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert!(agent(&first.snapshot, "x").parent.is_none());
    assert_eq!(agent(&first.snapshot, "y").parent.as_deref(), Some("x"));
    db.touch("x", 1, "");
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "content on a cycle");
    // A node on a subagent's parent chain changes: structural.
    db.touch("x", 2, "DELETE FROM orchestration_v2_projection_nodes WHERE node_id='n-x'");
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "chain node removed");
    db.touch("y", 3, "DELETE FROM orchestration_v2_projection_subagents WHERE subagent_id='sy'");
    let next = poll(&mut reader, &db, NOW, &mut prev);
    incremental(&next, "cycle link removed");
    assert_eq!(agent(&next.snapshot, "y").parent.as_deref(), Some("x"));
    assert!(agent(&next.snapshot, "x").parent.is_none());
    assert_eq!(reader.mismatches(), 0);
}

/// One provider thread whose turns live on the parent, routed to whichever
/// agent `native` names. A pure subagent that carries the provider id wins
/// over the child alias (the oracle fills `native` from agents first); once
/// it is gone the alias takes the parent's history.
#[test]
fn a_shared_provider_thread_moves_its_history_with_the_route() {
    for check in [false, true] {
        let mut db = Db::new();
        support::parent_provider_child(&mut db);
        db.db.execute_batch(r#"
            INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,origin,status,updated_at,payload_json)
              VALUES('pure','parent','r','n','codex','pt','native','running','','{"id":"pure","threadId":"parent","runId":"r","providerThreadId":"pt","status":"running"}');
        "#).unwrap();
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(check);
        let mut prev = None;
        let first = poll(&mut reader, &db, NOW, &mut prev);
        assert!(agent(&first.snapshot, "child").history.is_empty());
        assert_eq!(agent(&first.snapshot, "pure").history.len(), 1);
        db.touch("parent", 1, r#"INSERT INTO orchestration_v2_projection_provider_turns(provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json) VALUES('t2','parent','pt','n',2,'running','2026-11-12T11:30:00.000Z','{"providerThreadId":"pt","model":"synthetic","tokenUsage":{"usedTokens":9,"maxTokens":100}}')"#);
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "turn on a shared provider");
        assert_eq!(agent(&next.snapshot, "pure").history.len(), 2);
        assert_eq!(agent(&next.snapshot, "pure").context.used_tokens, Some(9.));
        assert_eq!(agent(&next.snapshot, "parent").context.used_tokens, Some(9.));
        db.touch("parent", 2, "DELETE FROM orchestration_v2_projection_subagents WHERE subagent_id='pure'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "pure subagent removed");
        assert_eq!(agent(&next.snapshot, "child").history.len(), 2);
        assert!(next.snapshot.agents.iter().all(|a| a.id != "pure"));
        db.touch("parent", 3, "DELETE FROM orchestration_v2_projection_subagents WHERE subagent_id='s'");
        let next = poll(&mut reader, &db, NOW, &mut prev);
        incremental(&next, "alias removed");
        assert!(agent(&next.snapshot, "child").history.is_empty());
        assert_eq!(reader.mismatches(), 0);
    }
}

/// Shapes the cache does not prove are refused with their own reason, still
/// equal to the oracle, and served again once they are gone.
#[test]
fn unproved_link_shapes_are_refused_by_name() {
    use serde_json::json;
    let cases: [(&str, &str, FallbackReason); 3] = [
        (
            r#"INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('other','host','n','codex','native','running','','{"id":"other","threadId":"host","status":"running"}')"#,
            "DELETE FROM orchestration_v2_projection_subagents",
            FallbackReason::SubagentThreadCollision,
        ),
        (
            r#"INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('s','host','n','codex','native','running','','{"id":"s","threadId":"host","providerThreadId":"","status":"running"}')"#,
            "DELETE FROM orchestration_v2_projection_subagents",
            FallbackReason::CrossThreadAttribution,
        ),
        (
            "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.id','renamed') WHERE thread_id='other'",
            "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.id','other') WHERE thread_id='other'",
            FallbackReason::ThreadIdentityMismatch,
        ),
    ];
    for (shape, undo, reason) in cases {
        let mut db = Db::new();
        db.thread("host", 0, json!({}));
        db.thread("other", 0, json!({}));
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        poll(&mut reader, &db, NOW, &mut prev);
        db.touch("host", 1, shape);
        db.touch("other", 2, "");
        let refused = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(refused.served, Served::Full, "{reason:?}");
        assert_eq!(refused.counters.fallback_reason, Some(reason));
        db.touch("host", 3, "");
        let again = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(again.counters.fallback_reason, Some(reason), "still refused while the shape stays");
        db.touch("host", 4, undo);
        db.touch("other", 5, "");
        poll(&mut reader, &db, NOW, &mut prev);
        db.touch("host", 6, "");
        incremental(&poll(&mut reader, &db, NOW, &mut prev), &format!("{reason:?} gone"));
        assert_eq!(reader.mismatches(), 0);
    }
}

/// T3's node id is a primary key. Without one, two active rows may share an
/// id and the oracle's node map keeps whichever SQLite returns last: refused.
/// A row on an archived thread is outside that map and is no conflict.
#[test]
fn shared_node_ids_are_refused_only_between_active_threads() {
    use serde_json::json;
    let mut db = Db::new();
    db.db.execute_batch("DROP TABLE orchestration_v2_projection_nodes; CREATE TABLE orchestration_v2_projection_nodes(node_id TEXT, thread_id TEXT NOT NULL, run_id TEXT, parent_node_id TEXT, root_node_id TEXT NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL, payload_json TEXT NOT NULL)").unwrap();
    db.thread("host", 0, json!({}));
    db.thread("other", 0, json!({}));
    db.thread("gone", 0, json!({}));
    db.db.execute_batch("UPDATE orchestration_v2_projection_threads SET archived_at='x' WHERE thread_id='gone'; INSERT INTO orchestration_v2_projection_nodes VALUES('n','host',NULL,NULL,'n','root','running','{}'),('n','gone',NULL,'p','n','root','running','{}')").unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.touch("host", 1, "");
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "archived duplicate");
    db.touch("other", 2, "INSERT INTO orchestration_v2_projection_nodes VALUES('n','other',NULL,'q','n','root','running','{}')");
    let refused = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(refused.counters.fallback_reason, Some(FallbackReason::DuplicateNodeId));
    db.touch("other", 3, "DELETE FROM orchestration_v2_projection_nodes WHERE thread_id='other'");
    poll(&mut reader, &db, NOW, &mut prev);
    db.touch("host", 4, "");
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "duplicate gone");
    assert_eq!(reader.mismatches(), 0);
}

fn cached_item_rows(reader: &Reader) -> u64 {
    let account = reader.cache_account().unwrap();
    account["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|table| table["table"] == "turn_items")
        .unwrap()["rows"]
        .as_u64()
        .unwrap()
}

/// Thread `a` has an old run whose commands are out of the window and a newer
/// run, its latest. The old run is not selected, so the cache does not keep its
/// rows. A subagent `s` on thread `b` then names the old run: the event names
/// `b` only, so `a` is not re-read, and the old run's rows have to be read by
/// run id. They sit under node `s`, so they route to the subagent, whose run
/// they are. Removing the subagent drops them again; restoring it reads them
/// again.
fn unselected_run_is_read_when_another_thread_selects_it(self_check: bool) {
    let mut db = Db::new();
    db.write("a", 0);
    db.write("b", 1);
    db.touch("a", 2, r#"
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,completed_at,payload_json)
          VALUES('r-old','a',1,'codex','pa','completed','2020-01-01T00:00:00.000Z','2020-01-01T00:10:00.000Z',
          '{"id":"r-old","threadId":"a","status":"completed","startedAt":"2020-01-01T00:00:00.000Z"}');
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,payload_json)
          VALUES('r-new','a',2,'codex','pa','running','2026-11-12T11:00:00.000Z',
          '{"id":"r-new","threadId":"a","status":"running","startedAt":"2026-11-12T11:00:00.000Z"}');
        INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
          VALUES('old-1','a','r-old','s',1,'command_execution','completed','2020-01-01T00:01:00.000Z','{"input":"old-one"}'),
                ('old-2','a','r-old','s',2,'command_execution','failed','2020-01-01T00:02:00.000Z','{"input":"old-two"}'),
                ('old-3','a','r-old','s',3,'user_message','completed','2020-01-01T00:03:00.000Z','{}'),
                ('new-1','a','r-new',NULL,1,'command_execution','running','2026-11-12T11:00:01.000Z','{"input":"new-one"}');
    "#);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(self_check);
    let mut prev = None;
    let first = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(first.counters.fallback_reason, Some(FallbackReason::InitialBaseline));
    assert_eq!(commands_of(&first.snapshot, "a"), ["new-one"]);
    // Only the selected run's row is kept.
    assert_eq!(cached_item_rows(&reader), 1);

    let subagent = r#"
        INSERT INTO orchestration_v2_projection_subagents
          (subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,child_thread_id,origin,status,updated_at,payload_json)
          VALUES('s','b','r-old','sn','codex',NULL,NULL,'native','running','',
          '{"id":"s","threadId":"b","runId":"r-old","status":"running"}');
    "#;
    db.touch("b", 3, subagent);
    let selected = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(selected.served, Served::Incremental, "{:?}", selected.counters.fallback_reason);
    assert_eq!(selected.counters.rows_read, selected.counters.cache_rows_read);
    assert_eq!(commands_of(&selected.snapshot, "a"), ["new-one"]);
    assert_eq!(commands_of(&selected.snapshot, "s"), ["old-two", "old-one"]);
    // The old run is selected: its two commands are cached now. Its user
    // message is not an item and not an event, so no selection takes it.
    assert_eq!(cached_item_rows(&reader), 3);

    db.touch("b", 4, "DELETE FROM orchestration_v2_projection_subagents WHERE subagent_id='s'");
    let dropped = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(dropped.served, Served::Incremental, "{:?}", dropped.counters.fallback_reason);
    assert!(!dropped.snapshot.agents.iter().any(|agent| agent.id == "s"));
    assert_eq!(cached_item_rows(&reader), 1);

    // A write on `a` while its old run is out of the cache re-reads `a`: the old
    // rows are read and left out again, and nothing else changes.
    db.write("a", 5);
    let reread = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(reread.served, Served::Incremental, "{:?}", reread.counters.fallback_reason);
    assert_eq!(cached_item_rows(&reader), 1);

    db.touch("b", 6, subagent);
    let again = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(again.served, Served::Incremental, "{:?}", again.counters.fallback_reason);
    assert_eq!(commands_of(&again.snapshot, "s"), ["old-two", "old-one"]);
    assert_eq!(cached_item_rows(&reader), 3);
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn unselected_run_is_read_when_another_thread_selects_it_checked() {
    unselected_run_is_read_when_another_thread_selects_it(true);
}

#[test]
fn unselected_run_is_read_when_another_thread_selects_it_unchecked() {
    unselected_run_is_read_when_another_thread_selects_it(false);
}

// ── T3's migration ledger, and commits that add no event ─────────────────────────
//
// The reader counts the rows of `effect_sql_migrations`. T3's migrator adds one per
// migration, in the same transaction as the migration's own writes, and a migration is
// the only thing T3 does that changes the dashboard's tables without an event. So:
// a change in the count reads in full, and with the count unchanged a commit that adds
// no event is served from the cache.

fn log_write(db: &mut Db, real: bool, id: &str, step: u64) {
    if real {
        db.write_application(id, step);
    } else {
        db.write(id, step);
    }
}
fn title_of(snapshot: &Snapshot, id: &str) -> String {
    agent(snapshot, id).title.clone()
}
fn quiet(update: &t3_agents_core::live::Update, what: &str) {
    assert_eq!(update.served, Served::Incremental, "{what}: {:?}", update.counters.fallback_reason);
    assert_eq!(update.counters.fallback_reason, None, "{what}");
    assert!(!update.counters.feed_complete, "{what}: no event was appended");
    assert_eq!(update.counters.cache_rows_read, 0, "{what}: no projection row is read");
    assert_eq!(update.counters.threads_reread, 0, "{what}");
}

#[test]
fn a_data_only_migration_committed_with_a_thread_event_reads_in_full() {
    for real in [false, true] {
        let mut db = Db::new();
        log_write(&mut db, real, "a", 0);
        log_write(&mut db, real, "b", 0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        poll(&mut reader, &db, NOW, &mut prev);
        // A migration rewrites thread b with no event; an ordinary write on a lands
        // before the next poll. The event log names a alone.
        db.migrate(
            "RepairTitles",
            "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Repaired') WHERE thread_id='b'",
        );
        log_write(&mut db, real, "a", 1);
        let migrated = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(migrated.served, Served::Full, "real log {real}");
        assert_eq!(migrated.counters.fallback_reason, Some(FallbackReason::LedgerChanged));
        assert_eq!(title_of(&migrated.snapshot, "b"), "Repaired");
        // The full read is the new baseline.
        log_write(&mut db, real, "a", 2);
        incremental(&poll(&mut reader, &db, NOW, &mut prev), "after the migration");
        db.silent(3);
        quiet(&poll(&mut reader, &db, NOW, &mut prev), "a commit without an event after the migration");
        assert_eq!(reader.mismatches(), 0);
    }
}

#[test]
fn migrations_and_ledger_rows_read_in_full_once() {
    let mut db = Db::new();
    db.write_application("a", 0);
    db.write_application("b", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    // A data-only migration by itself.
    db.migrate(
        "RepairTitles",
        "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Repaired') WHERE thread_id='a'",
    );
    let alone = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(alone.counters.fallback_reason, Some(FallbackReason::LedgerChanged));
    assert_eq!(title_of(&alone.snapshot, "a"), "Repaired");
    // A ledger row that changes no data: one full read, then the cache again.
    db.migrate("AddIndexElsewhere", "");
    let row = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(row.served, Served::Full);
    assert_eq!(row.counters.fallback_reason, Some(FallbackReason::LedgerChanged));
    db.silent(1);
    quiet(&poll(&mut reader, &db, NOW, &mut prev), "after a ledger row");
    // A migration that changes the schema as well, with an ordinary write after it.
    db.migrate(
        "ProjectionThreadsLabColumn",
        "ALTER TABLE orchestration_v2_projection_threads ADD COLUMN lab_flag INTEGER;
         UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Again') WHERE thread_id='b'",
    );
    db.write_application("a", 1);
    let schema = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(schema.served, Served::Full);
    assert_eq!(schema.counters.fallback_reason, Some(FallbackReason::SchemaChanged));
    assert_eq!(title_of(&schema.snapshot, "b"), "Again");
    db.write_application("a", 2);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "after the schema change");
    db.silent(2);
    quiet(&poll(&mut reader, &db, NOW, &mut prev), "after the schema change");
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn commits_without_an_event_are_served_from_the_cache() {
    for real in [false, true] {
        let mut db = Db::new();
        log_write(&mut db, real, "a", 0);
        log_write(&mut db, real, "b", 0);
        db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('i','b',1,'error','failed','2026-11-12T12:00:00.000Z','{}')",[]).unwrap();
        // A commit before the first poll changes nothing about the first poll.
        db.silent(0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        let first = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(first.counters.fallback_reason, Some(FallbackReason::InitialBaseline));
        for step in 1..6 {
            db.silent(step);
            let u = poll(&mut reader, &db, NOW, &mut prev);
            quiet(&u, &format!("silent commit {step}, real log {real}"));
            assert_eq!(u.changed, Changed::Agents(vec![]));
        }
        // Unchanged, the next poll skips.
        assert_eq!(poll(&mut reader, &db, NOW, &mut prev).served, Served::Skipped);
        // A commit while the clock moves past the event's window: the clock is handled
        // as with no commit, and the event leaves the snapshot without a full read.
        db.silent(6);
        let expired = poll(&mut reader, &db, NOW + 10_800_001., &mut prev);
        assert_eq!(expired.served, Served::Incremental, "{:?}", expired.counters.fallback_reason);
        assert!(agent(&expired.snapshot, "b").events.is_empty());
        // And a commit with the clock going backwards reads in full.
        db.silent(7);
        let back = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(back.counters.fallback_reason, Some(FallbackReason::BackwardsClock));
        assert_eq!(agent(&back.snapshot, "b").events.len(), 1);
        // Thread events still follow as before.
        log_write(&mut db, real, "b", 8);
        incremental(&poll(&mut reader, &db, NOW, &mut prev), "a thread event after the commits");
        assert_eq!(reader.mismatches(), 0);
    }
}

#[test]
fn a_commit_without_an_event_reads_in_full_without_a_countable_ledger() {
    // No ledger table: today's rule.
    let mut db = Db::without_ledger();
    db.write_application("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.silent(1);
    let none = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(none.served, Served::Full);
    assert_eq!(none.counters.fallback_reason, Some(FallbackReason::NoNewEvents));
    // A project event is not taken for a project tail either.
    db.project_event("p", "Renamed");
    let project = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(project.counters.fallback_reason, Some(FallbackReason::WatermarkMismatch));
    // A ledger whose count cannot be read is treated as none.
    db.db
        .execute_batch("CREATE VIEW effect_sql_migrations AS SELECT 1 AS migration_id WHERE json('x') IS NOT NULL")
        .unwrap();
    db.write_application("a", 2);
    assert_eq!(
        poll(&mut reader, &db, NOW, &mut prev).counters.fallback_reason,
        Some(FallbackReason::SchemaChanged)
    );
    db.write_application("a", 3);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "a thread event with an unreadable ledger");
    db.silent(4);
    let unreadable = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(unreadable.served, Served::Full);
    assert_eq!(unreadable.counters.fallback_reason, Some(FallbackReason::NoNewEvents));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn a_commit_without_an_event_and_a_noncanonical_cutoff_reads_in_full() {
    let mut db = Db::new();
    db.write("a", 0);
    db.db.execute("INSERT INTO orchestration_v2_projection_turn_items(turn_item_id,thread_id,ordinal,type,status,updated_at,payload_json) VALUES('i','a',1,'error','failed','2026-11-12T12:00:00Z','{}')",[]).unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.silent(1);
    let u = poll(&mut reader, &db, NOW + 1., &mut prev);
    assert_eq!(u.served, Served::Full);
    assert_eq!(u.counters.fallback_reason, Some(FallbackReason::NoNewEvents));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn project_events_above_the_cursor_are_served_from_the_cache() {
    let mut db = Db::new();
    db.write_application("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    // T3 logs a project change without moving the cursor: the log now runs past it.
    db.project_event("p", "Renamed");
    let renamed = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(renamed.served, Served::Incremental, "{:?}", renamed.counters.fallback_reason);
    assert!(renamed.counters.feed_complete);
    assert_eq!(renamed.counters.changed_threads, 0);
    // While it does, a commit without an event is still served from the cache.
    db.silent(1);
    quiet(&poll(&mut reader, &db, NOW, &mut prev), "a commit above a project tail");
    db.project_event("p", "Renamed again");
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "a second project event");
    // The next thread event moves the cursor past both project events.
    db.write_application("a", 1);
    let thread = poll(&mut reader, &db, NOW, &mut prev);
    incremental(&thread, "the thread event after the project events");
    assert_eq!(thread.counters.changed_threads, 1);
    // A project event and a thread event between two polls.
    db.project_event("p", "Renamed a third time");
    db.write_application("a", 2);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "a project and a thread event together");
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn only_project_events_may_run_past_the_cursor() {
    let mut db = Db::new();
    db.write_application("a", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.project_event("p", "Renamed");
    // A thread event the projection has not applied: T3 never commits one.
    db.db.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) VALUES('unapplied','thread','a',99,'synthetic','','system','{}','{}',2)",[]).unwrap();
    let u = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(u.served, Served::Full);
    assert_eq!(u.counters.fallback_reason, Some(FallbackReason::WatermarkMismatch));
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn prune_marks_moving_without_an_event_read_in_full() {
    let mut db = Db::new();
    for step in 0..8 {
        db.write_application("a", step);
    }
    db.db.execute_batch("INSERT INTO orchestration_v2_projection_metadata VALUES('event-log-prune-mark',2,2,''); INSERT INTO orchestration_v2_projection_metadata VALUES('event-log-pruned-through',2,2,'')").unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    // T3's prune: events deleted and both marks moved, in commits with no event.
    db.db.execute_batch("UPDATE orchestration_v2_projection_metadata SET last_sequence=5 WHERE projection_name='event-log-pruned-through'; DELETE FROM orchestration_events WHERE sequence < 5; UPDATE orchestration_v2_projection_metadata SET last_sequence=5 WHERE projection_name='event-log-prune-mark'").unwrap();
    let pruned = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(pruned.served, Served::Full);
    assert_eq!(pruned.counters.fallback_reason, Some(FallbackReason::PruningMarkerChanged));
    // Deleting events far below the cursor with the marks left alone changes nothing read.
    db.db.execute("DELETE FROM orchestration_events WHERE sequence = 5", []).unwrap();
    quiet(&poll(&mut reader, &db, NOW, &mut prev), "a delete below the marks");
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn a_commit_without_an_event_after_a_fallback_follows_the_fallback() {
    // After a text drift the cache is kept and the next poll reads the text again.
    let mut db = Db::new();
    db.write("a", 0);
    insert_command(&db.db, "i", "a", 1, &item_stamp(1), "synthetic");
    db.write("a", 1);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    reader.force_text_drift();
    db.write("a", 2);
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).counters.fallback_reason, Some(FallbackReason::TextDrift));
    db.silent(3);
    let next = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(next.served, Served::Incremental, "{:?}", next.counters.fallback_reason);
    assert!(commands_of(&next.snapshot, "a").iter().any(|c| c == "synthetic"));
    // A link shape the cache refuses stays refused on a commit without an event.
    use serde_json::json;
    let mut db = Db::new();
    db.thread("host", 0, json!({}));
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    db.touch("host", 1, r#"INSERT INTO orchestration_v2_projection_subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,updated_at,payload_json) VALUES('s','host','n','codex','native','running','','{"id":"s","threadId":"host","providerThreadId":"","status":"running"}')"#);
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).counters.fallback_reason, Some(FallbackReason::CrossThreadAttribution));
    db.silent(2);
    let refused = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(refused.served, Served::Full);
    assert_eq!(refused.counters.fallback_reason, Some(FallbackReason::CrossThreadAttribution));
    assert_eq!(reader.mismatches(), 0);
}

// T3 restores a snapshot of the database in place when an update fails. On Linux the
// file keeps its identity, so the reader sees it only through what the log and the
// ledger say.

#[cfg(unix)]
#[test]
fn a_restored_older_file_reads_in_full() {
    // The trial wrote events the reader saw: the cursor goes backwards.
    let mut db = Db::new();
    db.write_application("a", 0);
    db.write_application("b", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    let snapshot = db.snapshot();
    db.write_application("a", 1);
    db.write_application("a", 2);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "the trial's events");
    db.restore(&snapshot);
    let restored = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(restored.served, Served::Full);
    assert_eq!(restored.counters.fallback_reason, Some(FallbackReason::CursorRegression));
    assert_eq!(title_of(&restored.snapshot, "a"), "Title 0");
    db.write_application("b", 1);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "after the restore");

    // The trial ran a migration the reader saw, and nothing else: the count goes back down.
    let snapshot = db.snapshot();
    db.migrate(
        "RepairTitles",
        "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Repaired')",
    );
    assert_eq!(poll(&mut reader, &db, NOW, &mut prev).counters.fallback_reason, Some(FallbackReason::LedgerChanged));
    db.restore(&snapshot);
    let restored = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(restored.counters.fallback_reason, Some(FallbackReason::LedgerChanged));
    assert_eq!(title_of(&restored.snapshot, "b"), "Title 1");
    assert_eq!(reader.mismatches(), 0);
}

#[cfg(unix)]
#[test]
fn a_restored_log_that_names_another_thread_at_the_last_event_reads_in_full() {
    for past in [false, true] {
        let mut db = Db::new();
        db.write_application("a", 0);
        db.write_application("b", 0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
        let mut prev = None;
        poll(&mut reader, &db, NOW, &mut prev);
        let snapshot = db.snapshot();
        // The trial writes on a, and the reader sees it.
        db.write_application("a", 1);
        incremental(&poll(&mut reader, &db, NOW, &mut prev), "the trial's event");
        // Restored, and caught up before the next poll by writes on b: to the same
        // sequence, or past it.
        db.restore(&snapshot);
        db.write_application("b", 9);
        if past {
            db.write_application("b", 10);
        }
        let caught_up = poll(&mut reader, &db, NOW, &mut prev);
        assert_eq!(caught_up.served, Served::Full, "past {past}");
        assert_eq!(caught_up.counters.fallback_reason, Some(FallbackReason::AnchorReplaced), "past {past}");
        assert_eq!(title_of(&caught_up.snapshot, "a"), "Title 0");
        assert_eq!(reader.mismatches(), 0);
    }
}

/// Where the proof stops. An older file restored in place and caught up, before the
/// reader polls again, to an event at the very sequence it saw last that names the
/// same thread, with no migration the reader saw: nothing the reader may read differs
/// from a commit without an event (served from the cache), or from ordinary writes
/// (re-reading only the threads they name). Rows the trial changed stay as the trial
/// left them. The second case was so before the ledger; the first is new with it.
/// A poll between the restore and the catch-up sees the cursor go backwards.
#[cfg(unix)]
#[test]
fn a_restored_log_caught_up_on_the_same_thread_is_not_seen() {
    for past in [false, true] {
        let mut db = Db::new();
        db.write_application("a", 0);
        db.write_application("c", 0);
        let mut reader = Reader::open(&db.path).unwrap().with_self_check(false);
        reader.poll(NOW).unwrap();
        let snapshot = db.snapshot();
        // The trial: events on c, then on a. The reader sees both.
        db.write_application("c", 77);
        db.write_application("a", 1);
        assert_eq!(reader.poll(NOW).unwrap().served, Served::Incremental);
        // Restored, then two events bring the log back to the same sequence, the last on a.
        db.restore(&snapshot);
        db.write_application("b", 9);
        db.write_application("a", 9);
        if past {
            db.write_application("b", 10);
        }
        let stale = reader.poll(NOW).unwrap();
        let full = source::snapshot(&db.path, NOW).unwrap();
        assert_eq!(stale.served, Served::Incremental, "past {past}");
        assert_eq!(title_of(&full, "c"), "Title 0");
        assert_eq!(title_of(&stale.snapshot, "c"), "Title 77", "past {past}: the trial's row is still shown");
    }
}

/// Where T3 cannot clone the file, it copies the database while it runs and leaves
/// `-shm` out of the snapshot; restoring then deletes the live `-shm`. A kept-open
/// connection would keep reading the deleted one and skip every poll. The reader
/// notices the file beside the database change and opens it again.
#[cfg(unix)]
#[test]
fn a_restore_that_deletes_the_shared_memory_file_reopens_the_database() {
    let mut db = Db::new();
    db.write_application("a", 0);
    db.write_application("b", 0);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut prev = None;
    poll(&mut reader, &db, NOW, &mut prev);
    let snapshot = db.snapshot();
    std::fs::remove_file(snapshot.join("synthetic.sqlite-shm")).unwrap();
    db.write_application("c", 1);
    db.write_application("a", 1);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "the trial");
    assert_eq!(prev.as_ref().unwrap().agents.len(), 3);
    // Restored while T3 is stopped: the reader polls before the old version starts.
    db.put_back(&snapshot);
    let restored = poll(&mut reader, &db, NOW, &mut prev);
    assert_eq!(restored.served, Served::Full);
    assert_eq!(restored.counters.fallback_reason, Some(FallbackReason::DatabaseReplaced));
    assert_eq!(restored.snapshot.agents.len(), 2);
    db.reopen();
    db.write_application("b", 5);
    incremental(&poll(&mut reader, &db, NOW, &mut prev), "the old version's first write");
    db.silent(6);
    quiet(&poll(&mut reader, &db, NOW, &mut prev), "a commit without an event after the restore");
    assert_eq!(reader.mismatches(), 0);
}

/// The bar's status watcher reads through `Reader::status_only`: the same rules, every
/// agent reduced to `status_view`, compared here with the full reader's reduced the same way.
#[test]
fn the_status_only_reader_follows_the_ledger_and_commits_without_an_event() {
    let reduced = |db: &Db, now: f64| {
        let mut full = source::snapshot(&db.path, now).unwrap();
        for agent in &mut full.agents {
            t3_agents_core::live::status_view(Arc::make_mut(agent));
        }
        serde_json::to_value(&full).unwrap()
    };
    let check = |reader: &mut Reader, db: &Db, what: &str| {
        let u = reader.poll(NOW).unwrap();
        assert!(!u.mismatch, "{what}");
        assert_eq!(serde_json::to_value(&u.snapshot).unwrap(), reduced(db, NOW), "{what}");
        u
    };
    let mut db = Db::new();
    db.write_application("a", 0);
    db.write_application("b", 0);
    let mut reader = Reader::open(&db.path).unwrap().status_only().with_self_check(true);
    check(&mut reader, &db, "baseline");
    db.silent(1);
    assert_eq!(check(&mut reader, &db, "a commit without an event").served, Served::Incremental);
    db.project_event("p", "Renamed");
    assert_eq!(check(&mut reader, &db, "a project event").served, Served::Incremental);
    db.migrate(
        "RepairTitles",
        "UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.title','Repaired') WHERE thread_id='b'",
    );
    db.write_application("a", 1);
    let migrated = check(&mut reader, &db, "a migration with a thread event");
    assert_eq!(migrated.counters.fallback_reason, Some(FallbackReason::LedgerChanged));
    db.silent(2);
    assert_eq!(check(&mut reader, &db, "after the migration").served, Served::Incremental);
    assert_eq!(reader.mismatches(), 0);
}

#[test]
fn exact_runtime_asks_follow_the_request_node_and_activity_survives_message_mode() {
    use t3_agents_core::{
        model::{State, blocks_activity, needs_you, running, wants_attention},
        ui,
    };

    let mut db = Db::new();
    db.write("parent", 1);
    db.thread("child", 2, serde_json::json!({
        "lineage": {"relationshipToParent":"subagent", "parentThreadId":"parent"}
    }));
    db.touch("parent", 3, r#"
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
        VALUES
          ('run-p','parent',1,'codex','running','','{"id":"run-p","threadId":"parent","status":"running"}'),
          ('run-c','child',1,'codex','running','','{"id":"run-c","threadId":"child","status":"running"}');
        INSERT INTO orchestration_v2_projection_subagents
          (subagent_id,thread_id,parent_node_id,child_thread_id,provider,origin,status,updated_at,payload_json)
        VALUES ('delegate','parent','root','child','codex','native','running','',
          '{"id":"delegate","threadId":"parent","childThreadId":"child","status":"running"}');
        INSERT INTO orchestration_v2_projection_runtime_requests
          (runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json)
        VALUES
          ('ask','parent','delegate','user_input','pending','2026-11-12T11:01:00Z',NULL,
            '{"responseCapability":{"type":"message","providerSessionId":"SYNTHETIC_PRIVATE"},"questions":["SYNTHETIC_PRIVATE"]}'),
          ('approval','parent','root','approval','open','2026-11-12T11:01:00Z',NULL,
            '{"responseCapability":{"type":"message"}}'),
          ('wrong-state','parent','root','user_input','open','2026-11-12T11:01:00Z',NULL,'{}'),
          ('resolved','parent','root','user_input','pending','2026-11-12T11:01:00Z','done','{}');
        INSERT INTO orchestration_v2_projection_turn_items
          (turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
        VALUES
          ('working','child','run-c',NULL,1,'command_execution','running','2026-11-12T11:02:00Z','{}'),
          ('question','child','run-c',NULL,2,'user_input_request','waiting','2026-11-12T11:03:00Z',
            '{"questions":["SYNTHETIC_PRIVATE"]}');
    "#);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let initial = poll(&mut reader, &db, NOW, &mut previous);
    let parent = |s: &Snapshot| Arc::clone(s.agents.iter().find(|a| a.id == "parent").unwrap());
    let child = |s: &Snapshot| Arc::clone(s.agents.iter().find(|a| a.id == "child").unwrap());
    let agent = child(&initial.snapshot);
    assert_eq!(agent.parent.as_deref(), Some("parent"));
    assert_eq!(agent.open_asks.nonblocking, 1);
    assert_eq!(agent.open_asks.blocking, 0);
    assert_eq!(agent.asked.as_deref(), Some("2026-11-12T11:01:00Z"));
    assert!(agent.requests.is_empty());
    assert!(wants_attention(&agent) && running(&agent) && !needs_you(&agent) && !blocks_activity(&agent));
    assert_eq!(agent.now.as_ref().unwrap().kind, "command_execution");
    let root = parent(&initial.snapshot);
    assert_eq!(root.requests, ["approval"]);
    assert_eq!(root.open_asks, Default::default());
    assert!(needs_you(&root) && !running(&root));
    assert!(!format!("{initial:?}").contains("SYNTHETIC_PRIVATE"));
    assert_eq!(initial.served, Served::Full);
    let mut dashboard = State::new(Snapshot::default());
    dashboard.apply(initial.snapshot, &initial.changed);
    dashboard.pick_at("parent", NOW);
    dashboard.action("n");
    assert_eq!(dashboard.selected.as_deref(), Some("child"));
    let draw = |s: &State| {
        let (grid, frame) = ui::draw_grid(s, 120, 40, &s.palette);
        let row = frame.hits.iter().find(|hit| hit.id == "child").unwrap().rect.y as usize;
        let text = |cells: &[t3_agents_core::terminal::Cell]| -> String {
            cells.iter().map(|c| c.ch).collect()
        };
        (text(&grid.cells[row * 120..][..120]), text(&grid.cells[frame.detail.y as usize * 120..]))
    };
    let (row, detail) = draw(&dashboard);
    assert!(row.contains("◉") && row.contains("◇ 1 question open"), "{row}");
    assert!(detail.contains("RUNNING") && detail.contains("the run keeps going"));

    db.touch("parent", 4, r#"UPDATE orchestration_v2_projection_runtime_requests
        SET payload_json=json_set(payload_json,'$.responseCapability.type','live')
        WHERE runtime_request_id='ask'"#);
    let live = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(live.served, Served::Incremental);
    let agent = child(&live.snapshot);
    assert_eq!(agent.open_asks.blocking, 1);
    assert_eq!(agent.open_asks.nonblocking, 0);
    assert_eq!(agent.requests, ["user_input"]);
    assert!(needs_you(&agent) && !running(&agent) && blocks_activity(&agent));
    dashboard.apply(live.snapshot, &live.changed);
    let (row, detail) = draw(&dashboard);
    assert!(row.contains("◆") && !row.contains("◇"), "{row}");
    assert!(detail.contains("NEEDS YOU") && detail.contains("now  ◆ user input request"));

    db.touch("parent", 5, "UPDATE orchestration_v2_projection_runtime_requests SET status='cancelled' WHERE runtime_request_id='ask'");
    let cleared = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(cleared.served, Served::Incremental);
    let agent = child(&cleared.snapshot);
    assert_eq!(agent.open_asks, Default::default());
    assert!(running(&agent) && !needs_you(&agent));
    assert_eq!(parent(&cleared.snapshot).requests, ["approval"]);
    dashboard.apply(cleared.snapshot, &cleared.changed);
    assert!(draw(&dashboard).0.contains("◉"));
    dashboard.action("n");
    assert_eq!(dashboard.selected.as_deref(), Some("parent"));
}

#[test]
fn exact_cards_use_their_own_run_and_clear_links_only_on_later_human_messages() {
    use t3_agents_core::model::{AskCards, State, blocks_activity, needs_you, running, wants_attention};

    let mut db = Db::with_schema(include_str!("../../../checks/fixtures/t3-schema-main-096.sql"));
    db.write("a", 1);
    db.write("b", 2);
    db.touch("a", 3, r#"
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json)
        VALUES
          ('old','a',1,'codex','completed','','2026-11-10T11:03:00Z',
            '{"id":"old","threadId":"a","status":"completed"}'),
          ('secret-run','a',2,'codex','running','',NULL,
            '{"id":"secret-run","threadId":"a","status":"running"}'),
          ('latest','a',3,'codex','running','',NULL,
            '{"id":"latest","threadId":"a","status":"running"}');
        INSERT INTO orchestration_v2_projection_turn_items
          (turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
        VALUES
          ('dead-secret','a','old','n',1,'secret_request','waiting','2026-11-10T11:01:00Z','{}'),
          ('secret','a','secret-run','n',2,'secret_request','waiting','2026-11-10T11:01:00Z','{}'),
          ('link','a','old','n',3,'link_request','waiting','2026-11-10T11:03:00Z',
            '{"startedAt":"2026-11-10T11:01:00Z"}'),
          ('active-link','a','latest','n',4,'link_request','waiting','2026-11-12T11:02:00Z',
            '{"title":"SYNTHETIC_PRIVATE","label":"SYNTHETIC_PRIVATE","url":"SYNTHETIC_PRIVATE"}'),
          ('work','a','latest','n',5,'command_execution','running','2026-11-12T11:01:00Z','{}');
        INSERT INTO orchestration_v2_projection_messages
          (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
        VALUES
          ('equal','a','user',0,'2026-11-10T11:01:00Z','2026-11-10T11:01:00Z','{"createdBy":"user","text":"SYNTHETIC_PRIVATE"}'),
          ('assistant','a','assistant',0,'2026-11-12T11:55:00Z','2026-11-12T11:55:00Z','{"createdBy":"user"}'),
          ('agent','a','user',0,'2026-11-12T11:55:00Z','2026-11-12T11:55:00Z','{"createdBy":"agent"}'),
          ('unknown','a','user',0,'2026-11-12T11:55:00Z','2026-11-12T11:55:00Z','{}'),
          ('compaction','a','user',0,'2026-11-12T11:55:00Z','2026-11-12T11:55:00Z','{"createdBy":"user","idleCompaction":"beforeCacheExpires"}'),
          ('other','b','user',0,'2026-11-12T11:55:00Z','2026-11-12T11:55:00Z','{"createdBy":"user"}');
    "#);
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    let mut previous = None;
    let a = |s: &Snapshot| Arc::clone(s.agents.iter().find(|a| a.id == "a").unwrap());
    let initial = poll(&mut reader, &db, NOW, &mut previous);
    let agent = a(&initial.snapshot);
    assert_eq!(agent.open_asks.cards, AskCards { secret: 1, link: 2 });
    assert!(needs_you(&agent) && blocks_activity(&agent) && !running(&agent));
    assert_eq!(agent.now.as_ref().unwrap().kind, "command_execution");
    assert!(!format!("{initial:?}").contains("SYNTHETIC_PRIVATE"));

    db.write("b", 4);
    let unrelated = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(unrelated.served, Served::Incremental);
    assert!(Arc::ptr_eq(&agent, &a(&unrelated.snapshot)));

    db.touch("a", 5, "UPDATE orchestration_v2_projection_runs SET status='completed',
        completed_at='2026-11-12T11:53:00Z' WHERE run_id='secret-run'");
    let ended = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(ended.served, Served::Incremental);
    let agent = a(&ended.snapshot);
    assert_eq!(agent.open_asks.cards, AskCards { secret: 0, link: 2 });
    assert!(wants_attention(&agent) && running(&agent) && !needs_you(&agent) && !blocks_activity(&agent));

    // The message event can clear a link without changing any cached turn item
    // or the thread shell itself.
    db.db.execute_batch(r#"
        BEGIN;
        INSERT INTO orchestration_v2_projection_messages
          (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
        VALUES ('follow-up','a','user',0,'2026-11-10T11:01:00Z','2026-11-10T11:02:00Z','{"createdBy":"user","text":"SYNTHETIC_PRIVATE"}');
        INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json)
          VALUES('reply','a','message.updated','','{}');
        UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events)
          WHERE projection_name='thread-projections';
        COMMIT;
    "#).unwrap();
    let replied = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(replied.served, Served::Incremental);
    assert_eq!(a(&replied.snapshot).open_asks.cards.link, 1);
    assert_eq!(replied.changed, Changed::Agents(vec!["a".into()]));

    db.touch("a", 7, "UPDATE orchestration_v2_projection_runs SET status='completed',
        completed_at='2026-11-12T11:54:00Z', payload_json=json_set(payload_json,'$.status','completed')
        WHERE run_id='latest'");
    let idle = poll(&mut reader, &db, NOW, &mut previous);
    let agent = a(&idle.snapshot);
    assert_eq!(agent.open_asks.cards.link, 1);
    assert!(needs_you(&agent) && !running(&agent));
    let mut state = State::new(idle.snapshot);
    state.scope = "active".into();
    assert!(state.rows().iter().any(|row| row.id == "a"));

    db.touch("a", 8, "UPDATE orchestration_v2_projection_turn_items SET status='completed'
        WHERE turn_item_id='active-link'");
    let linked = poll(&mut reader, &db, NOW, &mut previous);
    assert_eq!(linked.served, Served::Incremental);
    let agent = a(&linked.snapshot);
    assert_eq!(agent.open_asks, Default::default());
    assert!(!needs_you(&agent) && !wants_attention(&agent));
    let mut state = State::new(linked.snapshot);
    state.scope = "active".into();
    assert!(state.rows().iter().all(|row| row.id != "a"));
}

/// The live cache keeps full turn JSON only for each provider thread's latest turn. An older
/// turn's token count is still history, so an edit to it must reach the next poll.
#[test]
fn editing_a_non_latest_turn_refreshes_context_history() {
    let mut db = Db::new();
    support::parent_provider_child(&mut db);
    db.touch(
        "parent",
        2,
        r#"INSERT INTO orchestration_v2_projection_provider_turns
            (provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json)
            VALUES('turn2','parent','pt','n',2,'running','2026-11-12T11:01:00.000Z',
            '{"providerThreadId":"pt","model":"synthetic","tokenUsage":{"usedTokens":2,"maxTokens":100}}')"#,
    );
    let old_usage = |snapshot: &Snapshot| {
        let child = snapshot.agents.iter().find(|a| a.id == "child").unwrap();
        assert_eq!(child.history.len(), 2);
        child.history.iter().min_by(|a, b| a.at.total_cmp(&b.at)).unwrap().used
    };
    let mut reader = Reader::open(&db.path).unwrap();
    assert_eq!(old_usage(&reader.poll(NOW).unwrap().snapshot), 1.);
    db.touch(
        "parent",
        3,
        "UPDATE orchestration_v2_projection_provider_turns
         SET payload_json=json_set(payload_json,'$.tokenUsage.usedTokens',70)
         WHERE provider_turn_id='turn'",
    );
    let expected = source::snapshot(&db.path, NOW).unwrap();
    assert_eq!(old_usage(&expected), 70.);
    let updated = reader.poll(NOW).unwrap();
    assert_eq!(updated.served, Served::Incremental);
    equal(&updated.snapshot, &expected);
}
