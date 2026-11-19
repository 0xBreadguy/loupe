mod support;
use support::Db;
use t3_agents_core::{
    live::{Reader, Served},
    model::{Changed, Snapshot, wants_attention},
    source,
};

const NOW: f64 = 1_794_483_000_000.;

fn estimate(s: &Snapshot, id: &str) -> u64 {
    let a = s.agents.iter().find(|a| a.id == id).unwrap();
    assert!(!wants_attention(a));
    a.estimated_asks
}

#[test]
fn estimates_follow_message_only_events_on_pinned_schemas() {
    for schema in [
        include_str!("../../../checks/fixtures/t3-schema-main-096.sql"),
        include_str!("../../../checks/fixtures/t3-schema-replayed-083.sql"),
        include_str!("../../../checks/fixtures/t3-schema-older-074.sql"),
    ] {
        check_estimates(schema);
    }
}

fn check_estimates(schema: &str) {
    let mut db = Db::with_schema(schema);
    db.write("a", 1);
    db.write("b", 2);
    // Hide the real table for the initial read, then restore it with its indexes.
    db.db.execute_batch("ALTER TABLE orchestration_v2_projection_messages RENAME TO hidden_messages").unwrap();
    let mut reader = Reader::open(&db.path).unwrap().with_self_check(true);
    assert_eq!(estimate(&reader.poll(NOW).unwrap().snapshot, "a"), 0);
    db.touch("a", 3, r#"
      ALTER TABLE hidden_messages RENAME TO orchestration_v2_projection_messages;
      INSERT INTO orchestration_v2_projection_messages
        (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
      VALUES
        ('a0','a','assistant',0,'2026-11-11T00:00:00.000Z','','{"text":"Should I go?"}'),
        ('human','a','user',0,'2026-11-12T00:00:00.000Z','','{"createdBy":"user","text":"PRIVATE-NONASSISTANT?"}'),
        ('a1','a','assistant',0,'2026-11-12T00:00:00.000Z','','{"text":"Should I go? Should I keep it?"}'),
        ('a2','a','assistant',0,'2026-11-12T00:00:00.000Z','','{"text":"Should I go?"}'),
        ('a3','a','assistant',0,'2026-11-12T00:01:00.000Z','','{"text":"Finished."}'),
        ('a4','a','assistant',0,'2026-11-12T00:02:00.000Z','','{"text":"Should I go? Should I keep it? Should I ship?","phase":"partial_answer"}'),
        ('a5','a','assistant',1,'2026-11-12T00:03:00.000Z','','{"text":"Should I do it?"}'),
        ('a6','a','assistant',0,'2026-11-12T00:04:00.000Z','','{"text":{"object":"Should I do it?"}}'),
        ('a7','a','assistant',0,'2026-11-12T00:05:00.000Z','','{invalid'),
        ('a8','a','assistant',0,'invalid','','{"text":"Should I do it?"}'),
        ('a9','a','assistant',0,'2026-11-12T00:06:00.000Z','','{"text":"Should I do it?","phase":"partial_answer"}'),
        ('a10','a','assistant',0,'2026-11-12T00:06:30.000Z','','{"text":"Should I do it?"}'),
        ('auto','a','user',0,'2026-11-12T00:07:00.000Z','','{"createdBy":"agent","text":"PRIVATE-NONASSISTANT?"}'),
        ('compact','a','user',0,'2026-11-12T00:08:00.000Z','','{"createdBy":"user","idleCompaction":{},"text":"PRIVATE-NONASSISTANT?"}'),
        ('system','a','system',0,'2026-11-12T00:09:00.000Z','','{"text":"PRIVATE-NONASSISTANT?"}'),
        ('b0','b','assistant',0,'2026-11-12T00:00:00.000Z','','{"text":"Should I go?"}');
      UPDATE orchestration_v2_projection_messages SET node_id='assistant-node' WHERE message_id='a4';
      INSERT INTO orchestration_v2_projection_turn_items
        (turn_item_id,thread_id,node_id,ordinal,type,status,updated_at,payload_json)
      VALUES
        ('assistant-item','a','assistant-node',1,'assistant_message','completed','','{"phase":"commentary","messageId":"a4","text":"PRIVATE-NONASSISTANT?"}'),
        ('older-item','a',NULL,2,'assistant_message','completed','','{"phase":"partial_answer","messageId":"a10"}');
    "#);
    assert_eq!(estimate(&source::snapshot(&db.path, NOW).unwrap(), "a"), 3);
    let full = reader.poll(NOW).unwrap();
    assert_eq!(full.served, Served::Full);
    assert_eq!(estimate(&full.snapshot, "a"), 3);
    assert_eq!(estimate(&full.snapshot, "b"), 1);
    assert!(!format!("{full:?}").contains("PRIVATE-NONASSISTANT"));

    db.touch(
        "b",
        4,
        "UPDATE orchestration_v2_projection_threads SET thread_id='b' WHERE thread_id='b'",
    );
    let unrelated = reader.poll(NOW).unwrap();
    assert_eq!(unrelated.served, Served::Incremental);
    assert_eq!(estimate(&unrelated.snapshot, "a"), 3);

    db.touch(
        "a",
        5,
        r#"
      INSERT INTO orchestration_v2_projection_runs
        (run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
      VALUES ('later','a',1,'codex','completed','2026-11-13T00:00:00.000Z',
        '{"id":"later","threadId":"a","status":"completed"}');
    "#,
    );
    assert_eq!(estimate(&reader.poll(NOW).unwrap().snapshot, "a"), 3);
    db.db.execute_batch(r#"
      BEGIN;
      INSERT INTO orchestration_v2_projection_messages
        (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
      VALUES
        ('reply','a','user',0,'2026-11-13T00:00:00.000Z','','{"createdBy":"user","text":"PRIVATE-NONASSISTANT?"}');
      INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json)
        VALUES('reply','a','message.updated','','{}');
      UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events)
        WHERE projection_name='thread-projections';
      COMMIT;
    "#).unwrap();
    let clear = reader.poll(NOW).unwrap();
    assert_eq!(clear.served, Served::Incremental);
    assert_eq!(clear.changed, Changed::Agents(vec!["a".into()]));
    assert_eq!(estimate(&clear.snapshot, "a"), 0);
    assert_eq!(estimate(&clear.snapshot, "b"), 1);
    assert_eq!(reader.mismatches(), 0);
}
