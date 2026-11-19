use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Db {
    pub path: PathBuf,
    pub db: Connection,
}
impl Db {
    pub fn new() -> Self {
        Self::with_schema(include_str!("../../../../checks/fixtures/t3-schema.sql"))
    }
    pub fn with_schema(schema: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "t3-live-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("synthetic.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(schema).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES('p','Synthetic','/synthetic','[]','',''); INSERT INTO orchestration_v2_projection_metadata VALUES('thread-projections',1,0,'');").unwrap();
        // A table T3 has that the dashboard never opens, for commits that add no event.
        db.execute_batch("CREATE TABLE orchestration_command_receipts (command_id TEXT PRIMARY KEY, status TEXT NOT NULL)").unwrap();
        Self { path, db }
    }
    /// A database without T3's migration ledger, as before T3's migrator first runs.
    /// The reader then treats every commit that adds no event as one that may have
    /// changed anything, and reads in full. Tests that change the dashboard's tables
    /// without an event, which T3 itself never does, use this.
    #[allow(dead_code)]
    pub fn without_ledger() -> Self {
        let db = Self::new();
        db.db.execute_batch("DROP TABLE effect_sql_migrations").unwrap();
        db
    }
    /// A migration as T3's migrator runs one: `sql` and the migration's ledger row in
    /// one transaction, with no event and no cursor move.
    #[allow(dead_code)]
    pub fn migrate(&mut self, name: &str, sql: &str) {
        let tx = self.db.transaction().unwrap();
        tx.execute_batch(sql).unwrap();
        tx.execute("INSERT INTO effect_sql_migrations(migration_id,name) VALUES((SELECT COALESCE(MAX(migration_id),0)+1 FROM effect_sql_migrations),?)", [name]).unwrap();
        tx.commit().unwrap();
    }
    /// A commit that adds no event and touches no table the dashboard reads, like T3
    /// writing its outbox, receipts or sessions.
    #[allow(dead_code)]
    pub fn silent(&mut self, step: u64) {
        self.db.execute("INSERT OR REPLACE INTO orchestration_command_receipts VALUES(?,'accepted')", [format!("cmd-{step}")]).unwrap();
    }
    /// A project change as T3 commits one (`commitProjectCommand`): a version-2
    /// project event in the real log and the project row, in one transaction. The
    /// projection's cursor does not move.
    #[allow(dead_code)]
    pub fn project_event(&mut self, project: &str, title: &str) {
        let tx = self.db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) VALUES(?,'project',?2,(SELECT COALESCE(MAX(stream_version),0)+1 FROM orchestration_events WHERE aggregate_kind='project' AND stream_id=?2),'project.meta-updated','','user','{}','{}',2)",params![format!("{project}-{title}"), project]).unwrap();
        tx.execute("UPDATE projection_projects SET title=? WHERE project_id=?", params![title, project]).unwrap();
        tx.commit().unwrap();
    }
    /// The database's three files, copied while nothing is writing: what T3's update
    /// launcher keeps before it starts a new version.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> PathBuf {
        let dir = self.path.parent().unwrap().join(format!("snapshot-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&dir).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{suffix}", self.path.display()));
            if from.exists() {
                std::fs::copy(&from, dir.join(format!("synthetic.sqlite{suffix}"))).unwrap();
            }
        }
        dir
    }
    /// Put a snapshot back the way T3 does on Linux when an update fails: the writer
    /// stops, and each file is copied over the live one in place (`cp --reflink=auto`
    /// into an existing file keeps its inode), so the file's identity does not change.
    /// A file the snapshot does not have is deleted. The writer stays closed.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn put_back(&mut self, snapshot: &std::path::Path) {
        use std::os::unix::fs::MetadataExt;
        let inode = std::fs::metadata(&self.path).unwrap().ino();
        drop(std::mem::replace(&mut self.db, Connection::open_in_memory().unwrap()));
        for suffix in ["", "-wal", "-shm"] {
            let to = PathBuf::from(format!("{}{suffix}", self.path.display()));
            let from = snapshot.join(format!("synthetic.sqlite{suffix}"));
            if from.exists() {
                std::fs::copy(&from, &to).unwrap();
            } else if to.exists() {
                std::fs::remove_file(&to).unwrap();
            }
        }
        assert_eq!(std::fs::metadata(&self.path).unwrap().ino(), inode, "the restore must keep the file's identity");
    }
    /// The writer starts again.
    #[allow(dead_code)]
    pub fn reopen(&mut self) {
        self.db = Connection::open(&self.path).unwrap();
    }
    /// `put_back`, then the writer starts again at once.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn restore(&mut self, snapshot: &std::path::Path) {
        self.put_back(snapshot);
        self.reopen();
    }
    pub fn write(&mut self, id: &str, step: u64) {
        let tx = self.db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) VALUES(?,?,'synthetic','','{\"prompt\":\"SECRET\"}')",params![format!("{id}-{step}"), id]).unwrap();
        tx.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json) VALUES(?,'p','Synthetic','codex','full-access','default','','',?) ON CONFLICT(thread_id) DO UPDATE SET payload_json=excluded.payload_json",params![id,json!({"id":id,"projectId":"p","title":format!("Title {step}"),"prompt":"SECRET"}).to_string()]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events) WHERE projection_name='thread-projections'",[]).unwrap();
        tx.commit().unwrap();
    }
    /// An event on `id` that keeps the thread row's payload and bumps its
    /// `updatedAt`, the way T3 marks recency on an ordinary write. `sql` runs in
    /// the same transaction first, so the event covers it.
    #[allow(dead_code)]
    pub fn touch(&mut self, id: &str, step: u64, sql: &str) {
        let tx = self.db.transaction().unwrap();
        tx.execute_batch(sql).unwrap();
        tx.execute("INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) VALUES(?,?,'synthetic','','{\"prompt\":\"SECRET\"}')",params![format!("{id}-{step}"), id]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.updatedAt',?) WHERE thread_id=?",params![format!("2026-11-12T11:{:02}:{:02}.000Z", step / 60 % 60, step % 60), id]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events) WHERE projection_name='thread-projections'",[]).unwrap();
        tx.commit().unwrap();
    }
    /// Insert or replace a thread row with this payload (its id and project are
    /// filled in), with an event on it.
    #[allow(dead_code)]
    pub fn thread(&mut self, id: &str, step: u64, payload: serde_json::Value) {
        let mut body = json!({"id": id, "projectId": "p", "title": id});
        if let (Some(body), Some(extra)) = (body.as_object_mut(), payload.as_object()) {
            for (key, value) in extra {
                body.insert(key.clone(), value.clone());
            }
        }
        let tx = self.db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) VALUES(?,?,'synthetic','','{\"prompt\":\"SECRET\"}')",params![format!("{id}-{step}"), id]).unwrap();
        tx.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json) VALUES(?,'p','Synthetic','codex','full-access','default','','',?) ON CONFLICT(thread_id) DO UPDATE SET payload_json=excluded.payload_json",params![id, body.to_string()]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_v2_events) WHERE projection_name='thread-projections'",[]).unwrap();
        tx.commit().unwrap();
    }
    /// This database's machine, beside the file, as T3 writes it.
    #[allow(dead_code)]
    pub fn environment(&self, id: &str) {
        std::fs::write(self.path.parent().unwrap().join("environment-id"), format!("{id}\n")).unwrap();
    }
    /// The same change as `write`, logged the way a real installation logs it: in
    /// `orchestration_events`, as a version-2 event of the thread aggregate.
    #[allow(dead_code)]
    pub fn write_application(&mut self, id: &str, step: u64) {
        let tx = self.db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) VALUES(?,'thread',?2,(SELECT COALESCE(MAX(stream_version),0)+1 FROM orchestration_events WHERE aggregate_kind='thread' AND stream_id=?2),'synthetic','','system','{\"prompt\":\"SECRET\"}','{}',2)",params![format!("{id}-{step}"), id]).unwrap();
        tx.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json) VALUES(?,'p','Synthetic','codex','full-access','default','','',?) ON CONFLICT(thread_id) DO UPDATE SET payload_json=excluded.payload_json",params![id,json!({"id":id,"projectId":"p","title":format!("Title {step}"),"prompt":"SECRET"}).to_string()]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_events) WHERE projection_name='thread-projections'",[]).unwrap();
        tx.commit().unwrap();
    }
    /// An event in the real log that names something other than a version-2 thread.
    #[allow(dead_code)]
    pub fn log_application(&mut self, event: &str, kind: &str, stream: &str, version: i64) {
        let tx = self.db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) VALUES(?,?,?,(SELECT COALESCE(MAX(stream_version),0)+1 FROM orchestration_events WHERE aggregate_kind=?2 AND stream_id=?3),'synthetic','','system','{}','{}',?)",params![event, kind, stream, version]).unwrap();
        tx.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=(SELECT max(sequence) FROM orchestration_events) WHERE projection_name='thread-projections'",[]).unwrap();
        tx.commit().unwrap();
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}

/// Parent-owned provider history routed through a subagent alias to a child.
/// This intentionally includes the lab's parent-provider shape, not a claim
/// about what T3 currently writes.
#[allow(dead_code)]
pub fn parent_provider_child(db: &mut Db) {
    db.write("parent", 0);
    db.write("child", 1);
    db.db.execute_batch(r#"
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,payload_json)
          VALUES('r','parent',1,'codex','pt','running','2026-11-12T11:00:00.000Z',
          '{"id":"r","threadId":"parent","providerThreadId":"pt","status":"running","startedAt":"2026-11-12T11:00:00.000Z"}');
        INSERT INTO orchestration_v2_projection_provider_threads
          (provider_thread_id,thread_id,provider,status,updated_at,payload_json)
          VALUES('pt','parent','codex','running','','{"id":"pt"}');
        INSERT INTO orchestration_v2_projection_provider_turns
          (provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,started_at,payload_json)
          VALUES('turn','parent','pt','n',1,'running','2026-11-12T11:00:00.000Z',
          '{"providerThreadId":"pt","model":"synthetic","tokenUsage":{"usedTokens":1,"maxTokens":100}}');
        INSERT INTO orchestration_v2_projection_subagents
          (subagent_id,thread_id,run_id,parent_node_id,provider,provider_thread_id,child_thread_id,origin,status,updated_at,payload_json)
          VALUES('s','parent','r','n','codex','pt','child','native','running','',
          '{"id":"s","threadId":"parent","runId":"r","providerThreadId":"pt","childThreadId":"child","status":"running"}');
    "#).unwrap();
}

/// A stamp inside the three-hour window of the tests' clock (2026-11-12T12:00:00Z), `i` milliseconds
/// after the start of the minute; larger `i` is later. Good for `i` up to 59,999.
#[allow(dead_code)]
pub fn stamp(i: i64) -> String {
    format!("2026-11-12T12:00:{:02}.{:03}Z", i / 1000, i % 1000)
}
