-- T3 Code's schema for the tables T3 Agents reads, plus its event log and projection metadata.
-- Extracted from the program bundle of T3 Code nightly 0.1.10-nightly.20261112.4580
-- (github.com/pingdotgg/t3code), not from anyone's database. It is a reference for building
-- realistic synthetic databases: the demo schema in oracle/src/demo.ts has none of these
-- indexes, so query plans measured on it are not T3's.
--
-- Approximate: statements are collected from the bundled migrations, not replayed in order.
-- Confirm against a real database's sqlite_master before relying on a specific index.

-- The event log a real installation appends to, confirmed on a real database on 2026-11-15:
-- every event, for every aggregate, lands here, and the projection's cursor in
-- orchestration_v2_projection_metadata is this table's newest sequence. T3 prunes it and
-- records how far in two more metadata rows, event-log-prune-mark and
-- event-log-pruned-through. orchestration_v2_events below exists in the same database and
-- stays empty.
CREATE TABLE orchestration_events ( sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, aggregate_kind TEXT NOT NULL, stream_id TEXT NOT NULL, stream_version INTEGER NOT NULL, event_type TEXT NOT NULL, occurred_at TEXT NOT NULL, command_id TEXT, causation_event_id TEXT, correlation_id TEXT, actor_kind TEXT NOT NULL, payload_json TEXT NOT NULL, metadata_json TEXT NOT NULL, application_event_version INTEGER NOT NULL DEFAULT 1 );
CREATE UNIQUE INDEX idx_orch_events_stream_version ON orchestration_events(aggregate_kind, stream_id, stream_version);
CREATE INDEX idx_orch_events_stream_sequence ON orchestration_events(aggregate_kind, stream_id, sequence);
CREATE INDEX idx_orch_events_command_id ON orchestration_events(command_id);
CREATE INDEX idx_orchestration_events_application_sequence ON orchestration_events(application_event_version, sequence);
CREATE INDEX idx_orchestration_events_application_high_water ON orchestration_events(sequence) WHERE aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread');
CREATE INDEX idx_orchestration_events_agent_stream_sequence ON orchestration_events(stream_id, sequence) WHERE application_event_version = 2 AND aggregate_kind = 'thread';
CREATE INDEX orchestration_events_v2_created_threads_idx ON orchestration_events(stream_id) WHERE application_event_version = 2 AND aggregate_kind = 'thread' AND event_type = 'thread.created';

CREATE TABLE orchestration_v2_events ( sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, command_id TEXT, thread_id TEXT NOT NULL, run_id TEXT, node_id TEXT, provider TEXT, raw_event_id TEXT, event_type TEXT NOT NULL, occurred_at TEXT NOT NULL, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_metadata ( projection_name TEXT PRIMARY KEY, schema_version INTEGER NOT NULL, last_sequence INTEGER NOT NULL, updated_at TEXT NOT NULL );

-- T3's migration ledger, created by the migrator of the `effect` library T3 uses
-- (effect/sql/Migrator, `table` defaults to effect_sql_migrations); this is its definition as it
-- stands on a real database (confirmed 2026-11-16). The migrator adds one row per migration in
-- the same transaction as the migration's own writes. The incremental reader reads only the
-- number of rows, no column: a migration can rewrite projection rows without logging an event.
-- Empty here; the scale lab fills it.
CREATE TABLE "effect_sql_migrations" ( migration_id integer PRIMARY KEY NOT NULL, created_at datetime NOT NULL DEFAULT current_timestamp, name VARCHAR(255) NOT NULL );

CREATE TABLE IF NOT EXISTS projection_projects ( project_id TEXT PRIMARY KEY, title TEXT NOT NULL, workspace_root TEXT NOT NULL, default_model TEXT, scripts_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT );

CREATE TABLE orchestration_v2_projection_threads ( thread_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL, default_provider TEXT NOT NULL, runtime_mode TEXT NOT NULL, interaction_mode TEXT NOT NULL, active_provider_thread_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, archived_at TEXT, deleted_at TEXT, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_runs ( run_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, ordinal INTEGER NOT NULL, provider TEXT NOT NULL, provider_thread_id TEXT, status TEXT NOT NULL, requested_at TEXT NOT NULL, completed_at TEXT, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_subagents ( subagent_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, run_id TEXT, parent_node_id TEXT NOT NULL, provider TEXT NOT NULL, provider_thread_id TEXT, child_thread_id TEXT, origin TEXT NOT NULL, status TEXT NOT NULL, started_at TEXT, completed_at TEXT, updated_at TEXT NOT NULL, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_nodes ( node_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, run_id TEXT, parent_node_id TEXT, root_node_id TEXT NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL, provider_thread_id TEXT, provider_turn_id TEXT, runtime_request_id TEXT, checkpoint_scope_id TEXT, started_at TEXT, completed_at TEXT, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_provider_threads ( provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, owner_node_id TEXT, provider TEXT NOT NULL, provider_session_id TEXT, status TEXT NOT NULL, first_run_ordinal INTEGER, last_run_ordinal INTEGER, updated_at TEXT NOT NULL, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_provider_turns ( provider_turn_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, provider_thread_id TEXT NOT NULL, node_id TEXT NOT NULL, run_attempt_id TEXT, ordinal INTEGER NOT NULL, status TEXT NOT NULL, started_at TEXT, completed_at TEXT, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_turn_items ( turn_item_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, run_id TEXT, node_id TEXT, provider_thread_id TEXT, provider_turn_id TEXT, parent_item_id TEXT, ordinal INTEGER NOT NULL, type TEXT NOT NULL, status TEXT NOT NULL, updated_at TEXT NOT NULL, payload_json TEXT NOT NULL );

CREATE TABLE orchestration_v2_projection_runtime_requests ( runtime_request_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, node_id TEXT NOT NULL, provider_turn_id TEXT, kind TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, resolved_at TEXT, payload_json TEXT NOT NULL );

ALTER TABLE projection_projects ADD COLUMN default_model_selection_json TEXT;
ALTER TABLE projection_projects DROP COLUMN default_model;
ALTER TABLE projection_projects ADD COLUMN default_thread_env_mode TEXT;
ALTER TABLE projection_projects ADD COLUMN favicon_path TEXT;
ALTER TABLE projection_projects ADD COLUMN auto_pull INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projection_projects ADD COLUMN project_icon_json TEXT;
ALTER TABLE orchestration_v2_events ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_events ADD COLUMN provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_threads ADD COLUMN provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_runs ADD COLUMN provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_projection_provider_threads ADD COLUMN provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN driver TEXT;
ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN provider_instance_id TEXT;
ALTER TABLE orchestration_v2_projection_subagents ADD COLUMN completion_delivery_state TEXT;
ALTER TABLE projection_projects ADD COLUMN pinned_at TEXT;
ALTER TABLE projection_projects ADD COLUMN archived_at TEXT;

CREATE INDEX IF NOT EXISTS idx_projection_projects_updated_at ON projection_projects(updated_at);
CREATE INDEX IF NOT EXISTS idx_projection_projects_workspace_root_deleted_at ON projection_projects(workspace_root, deleted_at);
CREATE INDEX orchestration_v2_events_command_idx ON orchestration_v2_events(command_id, sequence);
CREATE INDEX orchestration_v2_events_instance_sequence_idx ON orchestration_v2_events(provider_instance_id, sequence);
CREATE INDEX orchestration_v2_events_node_sequence_idx ON orchestration_v2_events(node_id, sequence);
CREATE INDEX orchestration_v2_events_raw_event_idx ON orchestration_v2_events(raw_event_id);
CREATE INDEX orchestration_v2_events_run_sequence_idx ON orchestration_v2_events(run_id, sequence);
CREATE INDEX orchestration_v2_events_thread_sequence_idx ON orchestration_v2_events(thread_id, sequence);
CREATE INDEX orchestration_v2_events_thread_type_sequence_idx ON orchestration_v2_events(thread_id, event_type, sequence);
CREATE INDEX IF NOT EXISTS orchestration_v2_projection_nodes_live_idx ON orchestration_v2_projection_nodes(thread_id) WHERE status IN ('pending', 'starting', 'running', 'waiting');
CREATE INDEX orchestration_v2_projection_nodes_parent_idx ON orchestration_v2_projection_nodes(parent_node_id);
CREATE INDEX orchestration_v2_projection_nodes_provider_turn_idx ON orchestration_v2_projection_nodes(provider_turn_id);
CREATE INDEX IF NOT EXISTS orchestration_v2_projection_nodes_settled_leaf_idx ON orchestration_v2_projection_nodes( thread_id, (kind IN ('tool_call', 'assistant_message', 'reasoning') AND status IN ('completed', 'interrupted', 'failed', 'cancelled', 'rolled_back') AND run_id IS NOT NULL) );
CREATE INDEX orchestration_v2_projection_nodes_thread_run_idx ON orchestration_v2_projection_nodes(thread_id, run_id);
CREATE INDEX orchestration_v2_projection_provider_threads_instance_status_idx ON orchestration_v2_projection_provider_threads(provider_instance_id, status);
CREATE INDEX orchestration_v2_projection_provider_threads_owner_idx ON orchestration_v2_projection_provider_threads(owner_node_id);
CREATE INDEX orchestration_v2_projection_provider_threads_session_idx ON orchestration_v2_projection_provider_threads(provider_session_id);
CREATE INDEX orchestration_v2_projection_provider_threads_thread_idx ON orchestration_v2_projection_provider_threads(thread_id);
CREATE INDEX orchestration_v2_projection_provider_turns_thread_idx ON orchestration_v2_projection_provider_turns(thread_id);
CREATE UNIQUE INDEX orchestration_v2_projection_provider_turns_thread_ordinal_idx ON orchestration_v2_projection_provider_turns(provider_thread_id, ordinal);
CREATE INDEX orchestration_v2_projection_requests_recovery_idx ON orchestration_v2_projection_runtime_requests(thread_id) WHERE status = 'pending';
CREATE INDEX orchestration_v2_projection_runs_delegated_delivery_idx ON orchestration_v2_projection_runs(thread_id) WHERE CASE WHEN json_valid(payload_json) THEN json_type(payload_json, '$.delegatedCompletion.delivery') = 'object' ELSE 0 END;
CREATE INDEX orchestration_v2_projection_runs_provider_thread_idx ON orchestration_v2_projection_runs(provider_thread_id);
CREATE INDEX orchestration_v2_projection_runs_recovery_idx ON orchestration_v2_projection_runs(status, thread_id) WHERE status IN ('queued', 'preparing', 'starting', 'running', 'waiting');
CREATE UNIQUE INDEX orchestration_v2_projection_runs_thread_ordinal_idx ON orchestration_v2_projection_runs(thread_id, ordinal);
CREATE INDEX orchestration_v2_projection_runs_thread_status_idx ON orchestration_v2_projection_runs(thread_id, status);
CREATE INDEX orchestration_v2_projection_runtime_requests_provider_turn_idx ON orchestration_v2_projection_runtime_requests(provider_turn_id);
CREATE INDEX orchestration_v2_projection_runtime_requests_thread_status_idx ON orchestration_v2_projection_runtime_requests(thread_id, status);
CREATE INDEX orchestration_v2_projection_subagents_child_thread_idx ON orchestration_v2_projection_subagents(child_thread_id);
CREATE INDEX orchestration_v2_projection_subagents_parent_node_idx ON orchestration_v2_projection_subagents(parent_node_id);
CREATE INDEX orchestration_v2_projection_subagents_provider_thread_idx ON orchestration_v2_projection_subagents(provider_thread_id);
CREATE INDEX orchestration_v2_projection_subagents_settled_idx ON orchestration_v2_projection_subagents(thread_id, status, completion_delivery_state, run_id);
CREATE INDEX orchestration_v2_projection_subagents_thread_idx ON orchestration_v2_projection_subagents(thread_id, started_at, subagent_id);
CREATE INDEX orchestration_v2_projection_threads_project_updated_idx ON orchestration_v2_projection_threads(project_id, updated_at);
CREATE INDEX orchestration_v2_projection_threads_subagent_result_idx ON orchestration_v2_projection_threads( thread_id, json_extract(payload_json, '$.lineage.parentThreadId') ) WHERE CASE WHEN json_valid(payload_json) THEN json_extract(payload_json, '$.lineage.relationshipToParent') = 'subagent' AND json_extract(payload_json, '$.lineage.parentThreadId') IS NOT NULL AND json_extract(payload_json, '$.forkedFrom.type') = 'node' ELSE 0 END;
CREATE INDEX orchestration_v2_projection_turn_items_active_ordinal_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id) WHERE status IN ('pending', 'running', 'waiting');
CREATE INDEX orchestration_v2_projection_turn_items_active_run_idx ON orchestration_v2_projection_turn_items(thread_id, run_id) WHERE status IN ('pending', 'running', 'waiting');
CREATE INDEX IF NOT EXISTS orchestration_v2_projection_turn_items_conversation_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, type, run_id) WHERE type NOT IN ( 'command_execution', 'reasoning', 'file_change', 'file_search', 'web_search', 'dynamic_tool' );
CREATE INDEX orchestration_v2_projection_turn_items_failed_error_idx ON orchestration_v2_projection_turn_items( thread_id, run_id, node_id, updated_at DESC, ordinal DESC, turn_item_id DESC ) WHERE type = 'error' AND status = 'failed';
CREATE INDEX orchestration_v2_projection_turn_items_node_ordinal_idx ON orchestration_v2_projection_turn_items(node_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_provider_turn_idx ON orchestration_v2_projection_turn_items(provider_turn_id);
CREATE INDEX orchestration_v2_projection_turn_items_recovery_idx ON orchestration_v2_projection_turn_items(thread_id) WHERE type IN ('command_execution', 'dynamic_tool', 'subagent') AND status IN ('pending', 'running', 'waiting');
CREATE INDEX orchestration_v2_projection_turn_items_run_ordinal_idx ON orchestration_v2_projection_turn_items(run_id, ordinal);
CREATE INDEX orchestration_v2_projection_turn_items_shell_pending_idx ON orchestration_v2_projection_turn_items(thread_id, run_id) WHERE type IN ('command_execution', 'dynamic_tool', 'subagent') AND status NOT IN ('completed', 'interrupted', 'failed', 'cancelled');
CREATE INDEX orchestration_v2_projection_turn_items_stop_request_idx ON orchestration_v2_projection_turn_items(thread_id, run_id) WHERE type = 'run_interrupt_request';
CREATE INDEX orchestration_v2_projection_turn_items_thread_ordinal_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id);
CREATE INDEX orchestration_v2_projection_turn_items_thread_run_idx ON orchestration_v2_projection_turn_items(thread_id, run_id);
CREATE INDEX IF NOT EXISTS orchestration_v2_projection_turn_items_user_message_idx ON orchestration_v2_projection_turn_items(thread_id, ordinal, turn_item_id) WHERE type = 'user_message';
CREATE INDEX IF NOT EXISTS orchestration_v2_projection_turn_items_waiting_link_idx ON orchestration_v2_projection_turn_items(thread_id, updated_at DESC, turn_item_id DESC) WHERE type = 'link_request' AND status = 'waiting';
CREATE INDEX orchestration_v2_provider_turns_codex_native_id_idx ON orchestration_v2_projection_provider_turns ( json_extract(payload_json, '$.nativeTurnRef.nativeId') ) WHERE json_extract(payload_json, '$.nativeTurnRef.driver') = 'codex';
