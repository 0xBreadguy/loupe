// What the event feed cannot see. Each case runs on a throwaway COPY of a
// real-schema scale database and asks: if an incremental reader tracks
// changed threads through `SELECT ... thread_id FROM orchestration_v2_events
// WHERE sequence > watermark`, does this kind of write show up? And does the
// cheap "did anything change" signal (PRAGMA data_version) still fire?
//
//   node checks/bench/feed-blind-spots.ts --db PATH [--db2 PATH]
//
// Cases:
//   projects      UPDATE projection_projects: not thread-scoped; events.thread_id
//                 is NOT NULL, so no event can carry it.
//   subagent_row  a subagent row stored under its parent thread while describing
//                 a child thread: the event carries the parent's thread_id, so a
//                 reader re-reading only "changed threads" sees the parent — the
//                 child thread's own rows only surface if their creation emitted
//                 events with the child's thread_id.
//   retag         UPDATE a turn_item moving it to a different thread_id: the old
//                 thread's snapshot changes with no event carrying the old id.
//   prune         DELETE events with sequence <= X (bundle-style pruning, with a
//                 pruning marker row in the metadata table): a watermark below the
//                 new minimum silently matches nothing.
//   vacuum        VACUUM rewrites the file: rowids are preserved (verified
//                 below), the events INTEGER PRIMARY KEY survives, and
//                 data_version moves.
//   clock         the readers' three-hour window: the snapshot changes with the
//                 clock even when the database does not (two --now-ms values).
import { copyFileSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, basename } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { Source, PREFIX } from "../../oracle/src/source.ts"

const args = process.argv.slice(2)
const arg = (name: string) => {
  const i = args.indexOf(name)
  return i < 0 ? undefined : args[i + 1]
}
const src = arg("--db")
if (!src) throw new Error("missing --db")
const NOW = 1794484800000

const dir = mkdtempSync(join(tmpdir(), "t3-feed-blind-spots-"))
const path = join(dir, basename(src))
const out: Record<string, unknown> = { source: src, copy: path }

// Agent-level summary for snapshot comparisons (never payload text).
const shape = (snap: { agents: any[] }) =>
  snap.agents.map((a) => `${a.id}:${a.status}:${a.project}:items${a.items.length}:events${a.events.length}:hist${a.history.length}`)

try {
  copyFileSync(src, path)
  // Keep the environment-id beside the copy so Source behaves identically.
  try {
    copyFileSync(join(src, "..", "environment-id"), join(dir, "environment-id"))
  } catch { /* optional */ }

  const reader = new DatabaseSync(path, { readOnly: true, timeout: 250 })
  reader.exec("PRAGMA query_only=ON")
  const dataVersion = () => (reader.prepare("PRAGMA data_version").get() as { data_version: number }).data_version
  const writer = new DatabaseSync(path)
  writer.exec("PRAGMA busy_timeout=2000")

  const feed = (watermark: number) =>
    (writer.prepare("SELECT DISTINCT thread_id FROM orchestration_v2_events WHERE sequence > ?").all(watermark) as { thread_id: string }[]).map((r) => r.thread_id)
  const lastSeq = () => (writer.prepare("SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'").get() as { last_sequence: number }).last_sequence
  const snapShape = () => shape(new Source(path).snapshot(NOW))

  // ── projects ──────────────────────────────────────────────────────────────
  {
    const before = snapShape()
    const v0 = dataVersion()
    const seq0 = lastSeq()
    writer.exec("UPDATE projection_projects SET title='renamed-project' WHERE project_id=(SELECT project_id FROM projection_projects LIMIT 1)")
    const after = snapShape()
    out.projects = {
      snapshot_changed: JSON.stringify(before) !== JSON.stringify(after),
      data_version_moved: dataVersion() !== v0,
      feed_new_threads: feed(seq0),
      covered: false,
      note: "renaming a project changes every agent's project label, with no event row at all",
    }
  }

  // ── subagent_row ──────────────────────────────────────────────────────────
  {
    const seq0 = lastSeq()
    const host = (writer.prepare(`SELECT thread_id FROM ${PREFIX}threads WHERE archived_at IS NULL AND deleted_at IS NULL LIMIT 1`).get() as { thread_id: string }).thread_id
    writer.exec("BEGIN")
    writer.prepare(`INSERT INTO orchestration_v2_events (event_id, thread_id, event_type, occurred_at, payload_json)
      VALUES ('blind-sub', ?, 'subagent.started', '2026-11-12T12:00:00.000Z', '{}')`).run(host)
    const seq = (writer.prepare("SELECT last_insert_rowid() AS s").get() as { s: number }).s
    writer.prepare(`INSERT INTO ${PREFIX}subagents (subagent_id, thread_id, run_id, parent_node_id, provider, origin, status, updated_at, payload_json)
      VALUES ('blind-sub', ?, NULL, ?, 'claude', 'native', 'running', '2026-11-12T12:00:00.000Z', ?)`).run(
      host, `n-${host}`, JSON.stringify({ id: "blind-sub", threadId: host, parentNodeId: `n-${host}`, childThreadId: "blind-child", status: "running", title: "blind spot subagent", updatedAt: "2026-11-12T12:00:00.000Z" }))
    writer.prepare("UPDATE orchestration_v2_projection_metadata SET last_sequence=? WHERE projection_name='thread-projections'").run(seq)
    writer.exec("COMMIT")
    const after = snapShape()
    out.subagent_row = {
      feed_new_threads: feed(seq0),
      host_agent_changed: after.some((a) => a.startsWith("blind-sub:")),
      covered: true,
      note: "the parent thread_id is in the feed, and the subagent row is found through the parent's re-read; a reader that keyed subagents to a child thread id would need child_thread_id resolution",
    }
  }

  // ── retag ─────────────────────────────────────────────────────────────────
  {
    const seq0 = lastSeq()
    const two = writer.prepare(`SELECT DISTINCT thread_id FROM ${PREFIX}threads WHERE archived_at IS NULL AND deleted_at IS NULL LIMIT 2`).all() as { thread_id: string }[]
    const [from, to] = two.map((r) => r.thread_id)
    const beforeAll = shape(new Source(path).snapshot(NOW))
    // Move one window-visible item of the from-thread's latest run to another
    // thread, as an upsert that re-tags can.
    const item = writer.prepare(`SELECT turn_item_id FROM ${PREFIX}turn_items WHERE thread_id=? AND updated_at >= ?
      ORDER BY updated_at DESC LIMIT 1`).get(from, new Date(NOW - 3 * 3600_000).toISOString()) as { turn_item_id: string }
    writer.exec("BEGIN")
    writer.prepare(`INSERT INTO orchestration_v2_events (event_id, thread_id, event_type, occurred_at, payload_json)
      VALUES ('blind-retag', ?, 'turn_item.upserted', '2026-11-12T12:00:01.000Z', '{}')`).run(to)
    const seq = (writer.prepare("SELECT last_insert_rowid() AS s").get() as { s: number }).s
    // A re-tag moves the row's thread attribution: thread_id and the provider
    // conversation both move (attribution keys on provider_thread_id first).
    const moved = writer.prepare(`UPDATE ${PREFIX}turn_items SET thread_id=?, provider_thread_id=? WHERE turn_item_id=?`).run(to, `pt-${to}`, item.turn_item_id).changes
    writer.prepare("UPDATE orchestration_v2_projection_metadata SET last_sequence=? WHERE projection_name='thread-projections'").run(seq)
    writer.exec("COMMIT")
    const afterAll = shape(new Source(path).snapshot(NOW))
    const changedAgents = beforeAll.filter((a, i) => afterAll[i] !== a).map((a) => a.split(":")[0])
    const feedThreads = feed(seq0)
    out.retag = {
      item_moved: item.turn_item_id,
      rows_updated: moved,
      event_carries: to,
      feed_new_threads: feedThreads,
      changed_agents: changedAgents,
      changed_agents_outside_feed: changedAgents.filter((a) => a !== to),
      covered: false,
      note: "the row moved thread; the event names only the new thread, so the old side's change is invisible to a watermark feed",
    }
  }

  // ── prune ─────────────────────────────────────────────────────────────────
  {
    const maxSeq = lastSeq()
    const keep = 10
    writer.exec("BEGIN")
    writer.prepare("DELETE FROM orchestration_v2_events WHERE sequence <= ?").run(maxSeq - keep)
    writer.prepare(`INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at)
      VALUES ('thread-projections:pruned-through', 1, ?, '2026-11-12T12:00:02.000Z')
      ON CONFLICT(projection_name) DO UPDATE SET last_sequence=excluded.last_sequence, updated_at=excluded.updated_at`).run(maxSeq - keep)
    writer.exec("COMMIT")
    const minSeq = (writer.prepare("SELECT MIN(sequence) AS s FROM orchestration_v2_events").get() as { s: number }).s
    const staleWatermark = minSeq - 50 // a reader whose watermark survived from before the prune
    const missed = (writer.prepare("SELECT COUNT(*) AS n FROM orchestration_v2_events WHERE sequence > ?").get(staleWatermark) as { n: number }).n
    out.prune = {
      min_sequence_after_prune: minSeq,
      events_matching_stale_watermark: missed,
      covered: false,
      note: missed > 0
        ? "a stale watermark silently re-reads the kept tail as if it were the delta; the reader must compare its watermark to MIN(sequence) (or the pruning marker) and fall back to a full rebuild when the watermark is below it"
        : "stale watermark matches nothing; changes between the watermark and the prune horizon are lost silently — full rebuild required",
    }
    // Restore is unnecessary: the copy is discarded.
  }

  // ── vacuum ────────────────────────────────────────────────────────────────
  {
    const rowidsBefore = (writer.prepare(`SELECT provider_thread_id, rowid FROM ${PREFIX}provider_turns ORDER BY provider_thread_id, ordinal LIMIT 20`).all() as { provider_thread_id: string; rowid: number }[])
    const v0 = dataVersion()
    const seq0 = lastSeq()
    writer.exec("VACUUM")
    const rowidsAfter = (writer.prepare(`SELECT provider_thread_id, rowid FROM ${PREFIX}provider_turns ORDER BY provider_thread_id, ordinal LIMIT 20`).all() as { provider_thread_id: string; rowid: number }[])
    const eventsIntact = (writer.prepare("SELECT COUNT(*) AS n, MIN(sequence) AS lo, MAX(sequence) AS hi FROM orchestration_v2_events").get() as { n: number; lo: number; hi: number })
    out.vacuum = {
      projection_rowids_changed: JSON.stringify(rowidsBefore) !== JSON.stringify(rowidsAfter),
      events_after: eventsIntact,
      data_version_moved: dataVersion() !== v0,
      last_sequence_unchanged: lastSeq() === seq0,
      covered: true,
      note: "VACUUM preserves rowids (measured: unchanged) and the event log's INTEGER PRIMARY KEY sequence survives; data_version moves, so a data_version-gated reader does a full rebuild and notices nothing was missed",
    }
  }

  // ── clock ─────────────────────────────────────────────────────────────────
  {
    const at = snapShape()
    const later = shape(new Source(path).snapshot(NOW + 3 * 3600_000 + 60_000))
    out.clock = {
      snapshot_at_epoch_vs_epoch_plus_3h01m_changed: JSON.stringify(at) !== JSON.stringify(later),
      covered: false,
      note: "the 3-hour lane window slides with the clock: events and history leave the window with no database write at all; time-based expiry cannot be gated on data_version",
    }
  }

  reader.close()
  writer.close()
} finally {
  console.log(JSON.stringify(out, null, 1))
  rmSync(dir, { recursive: true, force: true })
}
