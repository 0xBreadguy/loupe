// Does PRAGMA data_version tell a kept-open read-only connection that another
// connection wrote? This is the question an incremental reader cares about:
// keep the connection, poll data_version, rebuild only when it changes.
//
//   node checks/bench/data-version.ts --db PATH
//
// Runs the probe on a COPY of the database in a temporary directory, so the
// measured databases stay pristine. Uses only node:sqlite, like the reader.
// The write probe uses the turn_items table (present in both the demo and the
// real T3 schema); on the real schema every NOT NULL column is filled.
import { copyFileSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, basename } from "node:path"
import { DatabaseSync } from "node:sqlite"

const args = process.argv.slice(2)
const path = args[args.indexOf("--db") + 1]
if (!path) throw new Error("missing --db")

const dir = mkdtempSync(join(tmpdir(), "t3-scale-data-version-"))
const copy = join(dir, basename(path))
try {
  copyFileSync(path, copy)
  const dataVersion = (db: DatabaseSync) => (db.prepare("PRAGMA data_version").get() as { data_version: number }).data_version
  // A row the probe can insert and delete on either schema. turn_items is
  // present in both; fill the real schema's NOT NULL columns when they exist.
  const cols = (db: DatabaseSync, table: string) =>
    new Set((db.prepare(`SELECT name FROM pragma_table_info('${table}')`).all() as { name: string }[]).map((r) => r.name))
  const probeWrite = (db: DatabaseSync, id: string, deleteOnly: boolean) => {
    const table = "orchestration_v2_projection_turn_items"
    if (deleteOnly) {
      db.prepare(`DELETE FROM ${table} WHERE ${cols(db, table).has("turn_item_id") ? "turn_item_id" : "item_id"}=?`).run(id)
      return
    }
    if (cols(db, table).has("turn_item_id")) {
      db.prepare(`INSERT INTO ${table} (turn_item_id, thread_id, ordinal, type, status, updated_at, payload_json)
        VALUES (?, 'probe-thread', 0, 'notification', 'completed', '2026-11-12T12:00:00.000Z', '{}')`).run(id)
    } else {
      db.prepare(`INSERT INTO ${table} (item_id, thread_id, type, status, updated_at, ordinal, payload_json)
        VALUES (?, 'probe-thread', 'notification', 'completed', '2026-11-12T12:00:00.000Z', 0, '{}')`).run(id)
    }
  }

  // Reader opened exactly like Source.snapshot opens it, but kept open.
  const reader = new DatabaseSync(copy, { readOnly: true, timeout: 250 })
  reader.exec("PRAGMA query_only=ON")
  const v0 = dataVersion(reader)

  const writer = new DatabaseSync(copy)
  writer.exec("BEGIN")
  probeWrite(writer, "probe", false)
  writer.exec("COMMIT")

  const v1 = dataVersion(reader)
  // A snapshot-style read transaction in between, then another write.
  reader.exec("BEGIN")
  reader.exec("SELECT COUNT(*) FROM projection_projects")
  const vInsideTxn = dataVersion(reader)
  reader.exec("ROLLBACK")
  writer.exec("BEGIN")
  probeWrite(writer, "probe", true)
  writer.exec("COMMIT")
  const v2 = dataVersion(reader)
  // A write through the reader's own connection is not possible (query_only),
  // but same-connection commits are documented not to bump data_version; show
  // the writer's own view for contrast.
  writer.exec("BEGIN")
  probeWrite(writer, "probe2", false)
  writer.exec("COMMIT")
  const writerView = dataVersion(writer)
  const v3 = dataVersion(reader)

  console.log(JSON.stringify({
    path: copy,
    initial: v0,
    after_other_connection_commit: v1,
    inside_open_read_transaction: vInsideTxn,
    after_second_commit: v2,
    writer_own_view_after_own_commit: writerView,
    reader_after_writer_third_commit: v3,
    changed_for_reader: v0 !== v1 && v1 !== v2,
  }))
  reader.close()
  writer.close()
} finally {
  rmSync(dir, { recursive: true, force: true })
}
