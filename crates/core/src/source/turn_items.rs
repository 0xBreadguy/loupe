//! Single-pass leaf-only `turn_items` scan + forward-sorted rowid payload fetch
//! for bounded items (LIMIT 3000), events (LIMIT 8000 within 3h window), plans
//! (`todo_list`), and the `AsstItemIndex` hint map.
//!
//! Deliberate difference from `oracle/` (Decision 1): displaced rows outside
//! the 3,000-item / 8,000-event cut never have their `payload_json` parsed,
//! both on the primary leaf-scan path and on the two-phase key-only tie
//! fallback. In-cut rows still validate `payload_json` identically to `oracle/`.
use super::{
    ITEM_EXIT_CODE, PREFIX, SourceError, TOOLISH,
    asks_hint::{AsstItemIndex, AsstSlot},
    db_error, each_row_cached, each_row_uncached,
    fast_reads::{
        FetchedTurnItem, FnvHasher, PlanRow, RowidHasher, is_rowid_alias, ordered_columns,
        rowid_unshadowed, sql_opt_text, todo_lists,
    },
    item_fields, marks, optional, plan, str_at, strings, timing,
};
use crate::model::{self, Item};
use rusqlite::{
    Connection,
    types::{Value as SqlValue, ValueRef},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

fn turn_items_leaf_scan_safe(db: &Connection) -> rusqlite::Result<bool> {
    let table = format!("{PREFIX}turn_items");
    let cols = ordered_columns(db, &table)?;
    if cols.last().map(String::as_str) != Some("payload_json")
        || cols.iter().any(|c| is_rowid_alias(c))
    {
        return Ok(false);
    }
    let col_set: HashSet<&str> = cols.iter().map(String::as_str).collect();
    for needed in [
        "thread_id",
        "run_id",
        "node_id",
        "provider_thread_id",
        "type",
        "status",
        "updated_at",
        "ordinal",
        "payload_json",
    ] {
        if !col_set.contains(needed) {
            return Ok(false);
        }
    }
    let scan_sql =
        format!("SELECT rowid, thread_id, run_id, node_id, type, updated_at, ordinal FROM {table}");
    let steps = plan(db, &scan_sql, &[])?;
    Ok(steps.len() == 1 && steps[0].1 == format!("SCAN {table}"))
}

#[derive(Clone, Eq, PartialEq)]
enum CompactStamp {
    Null,
    Iso24([u8; 24]),
    Other(Box<[u8]>),
}

impl CompactStamp {
    #[inline]
    fn from_bytes(b: &[u8]) -> Self {
        if b.len() == 24 {
            let mut arr = [0u8; 24];
            arr.copy_from_slice(b);
            Self::Iso24(arr)
        } else {
            Self::Other(b.to_vec().into_boxed_slice())
        }
    }
    #[inline]
    fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Null => None,
            Self::Iso24(a) => Some(a.as_slice()),
            Self::Other(b) => Some(b),
        }
    }
}

impl Ord for CompactStamp {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(&other.as_bytes())
    }
}
impl PartialOrd for CompactStamp {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[inline]
fn is_toolish_bytes(kind: &[u8]) -> bool {
    matches!(
        kind,
        b"command_execution"
            | b"dynamic_tool"
            | b"file_change"
            | b"web_search"
            | b"subagent"
            | b"handoff"
            | b"notification"
            | b"user_input_request"
            | b"secret_request"
            | b"link_request"
            | b"reasoning"
            | b"assistant_message"
    )
}

#[derive(Eq, PartialEq)]
struct ItemRankEntry {
    updated_at: CompactStamp,
    ordinal: Option<i64>,
    /// Index into `run_ids` for non-null `run_id`, or `u32::MAX` for `run_id IS NULL`.
    /// Because `attach_item` matches `item.run_id == agent.run.id`, two items with
    /// distinct non-null `run_id`s can never land in the same agent's `items` slice.
    owner_key: u32,
    rowid: i64,
}

#[derive(Eq, PartialEq)]
struct EventRankEntry {
    updated_at: CompactStamp,
    rowid: i64,
}

fn events_ties_safe(events: &[FetchedTurnItem]) -> bool {
    let mut i = 0;
    while i < events.len() {
        let mut j = i + 1;
        while j < events.len() && events[j].updated_at == events[i].updated_at {
            j += 1;
        }
        if j - i > 1 {
            // Assembly stably sorts events by their displayed time and keeps
            // recent failures in query order. A tie matters only when it can
            // reorder distinguishable entries in either list.
            let mut failed = false;
            let mut by_time = HashMap::new();
            for event in &events[i..j] {
                let is_failed = event.status.as_deref() == Some("failed");
                let time = model::ms(if is_failed {
                    event.updated_at.as_deref()
                } else {
                    event.started_at.as_deref()
                })
                .or_else(|| model::ms(event.updated_at.as_deref()));
                let Some(time) = time else { continue };
                if is_failed && std::mem::replace(&mut failed, true) {
                    return false;
                }
                let category =
                    model::category(event.kind.as_deref().unwrap_or(""), event.tool.as_deref());
                let signature = (is_failed, category);
                if by_time
                    .insert(time.to_bits(), signature)
                    .is_some_and(|old| old != signature)
                {
                    return false;
                }
            }
        }
        i = j;
    }
    true
}

fn normalize_plan(steps: Vec<(Option<usize>, String)>) -> Vec<(Option<usize>, String)> {
    steps
        .into_iter()
        .map(|(p, detail)| (p, detail.replace("USING COVERING INDEX ", "USING INDEX ")))
        .collect()
}

/// Two-phase tie fallback for bounded `turn_items` queries: runs the exact same
/// `WHERE ... ORDER BY ... LIMIT N` without extracting JSON, then fetches `item_fields()`
/// only for the winning rowids in monotonic rowid order and re-orders them to
/// SQLite's exact output order.
///
/// Matching plans keep the visit order. Try a key-only sort first, then a
/// projection that references the same columns as the reference query so
/// covering indexes remain eligible in the same way.
fn fetch_by_rowid_two_phase(
    db: &Connection,
    path: &Path,
    tail: &str,
    oracle_sql: &str,
    args: &[SqlValue],
) -> Result<Option<Vec<FetchedTurnItem>>, SourceError> {
    let table = format!("{PREFIX}turn_items");
    if !rowid_unshadowed(db, &table).unwrap_or(false) {
        return Ok(None);
    }
    let oracle_plan = match plan(db, oracle_sql, args) {
        Ok(p) => normalize_plan(p),
        Err(_) => return Ok(None),
    };
    let projections = [
        "rowid",
        "rowid,thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,typeof(payload_json)",
    ];
    let key_sql = projections
        .iter()
        .map(|fields| format!("SELECT {fields} {tail}"))
        .find(|sql| plan(db, sql, args).is_ok_and(|p| normalize_plan(p) == oracle_plan));
    let Some(key_sql) = key_sql else {
        return Ok(None);
    };

    let mut ordered_rids: Vec<i64> = Vec::new();
    let mut usable = true;
    each_row_cached(db, &key_sql, args, |row| {
        match row.get_ref(0)? {
            ValueRef::Integer(rid) => ordered_rids.push(rid),
            _ => usable = false,
        }
        Ok(())
    })
    .map_err(|e| db_error(path, e))?;
    if !usable {
        return Ok(None);
    }
    if ordered_rids.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut sorted_rids = ordered_rids.clone();
    sorted_rids.sort_unstable();
    sorted_rids.dedup();

    let fields = item_fields();
    let mut by_rid: HashMap<i64, FetchedTurnItem, std::hash::BuildHasherDefault<RowidHasher>> =
        HashMap::with_capacity_and_hasher(sorted_rids.len(), Default::default());
    for chunk in sorted_rids.chunks(900) {
        let fetch_sql = format!(
            "SELECT rowid, {fields} FROM {table} WHERE rowid IN ({}) -- ti_fetch",
            marks(chunk.len())
        );
        let fetch_args: Vec<SqlValue> = chunk.iter().copied().map(SqlValue::Integer).collect();
        each_row_uncached(db, &fetch_sql, &fetch_args, |row| {
            if let ValueRef::Integer(rid) = row.get_ref(0)? {
                let item = FetchedTurnItem::from_row(row, 1, false)?;
                by_rid.insert(rid, item);
            }
            Ok(())
        })
        .map_err(|e| db_error(path, e))?;
    }
    if by_rid.len() != sorted_rids.len() {
        return Ok(None);
    }

    let mut out = Vec::with_capacity(ordered_rids.len());
    for rid in ordered_rids {
        if let Some(row) = by_rid.remove(&rid) {
            out.push(row);
        } else {
            return Ok(None);
        }
    }
    Ok(Some(out))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_turn_items_combined(
    db: &Connection,
    path: &Path,
    where_ids: &str,
    params: &[SqlValue],
    active_set: &HashSet<&str>,
    run_ids: &[String],
    runless: &[String],
    since: &str,
) -> Result<
    (
        Vec<Item>,
        Vec<FetchedTurnItem>,
        Vec<PlanRow>,
        Option<AsstItemIndex>,
        usize,
    ),
    SourceError,
> {
    let mut items_out: Option<Vec<Item>> = None;
    let mut events_out: Option<Vec<FetchedTurnItem>> = None;
    let mut plans_out: Option<Vec<PlanRow>> = None;
    let mut asst_index: Option<AsstItemIndex> = None;
    let mut total_rows = 0usize;

    let leaf_ok = turn_items_leaf_scan_safe(db).unwrap_or(false);
    if leaf_ok {
        let _scan_phase = timing::phase("read_items");
        let mut active_map: HashMap<&[u8], u32, std::hash::BuildHasherDefault<FnvHasher>> =
            HashMap::with_capacity_and_hasher(active_set.len(), Default::default());
        let mut tid_to_idx: HashMap<Box<[u8]>, u32, std::hash::BuildHasherDefault<FnvHasher>> =
            HashMap::with_capacity_and_hasher(active_set.len(), Default::default());
        for (idx, &tid) in active_set.iter().enumerate() {
            active_map.insert(tid.as_bytes(), idx as u32);
            tid_to_idx.insert(tid.as_bytes().to_vec().into_boxed_slice(), idx as u32);
        }
        let mut run_id_map: HashMap<&[u8], u32, std::hash::BuildHasherDefault<FnvHasher>> =
            HashMap::with_capacity_and_hasher(run_ids.len(), Default::default());
        for (idx, r) in run_ids.iter().enumerate() {
            run_id_map.insert(r.as_bytes(), idx as u32);
        }
        let mut runless_set: HashSet<&[u8], std::hash::BuildHasherDefault<FnvHasher>> =
            HashSet::with_capacity_and_hasher(runless.len(), Default::default());
        for t in runless {
            runless_set.insert(t.as_bytes());
        }
        let since_bytes = since.as_bytes();

        // Flat Vec + O(N) select_nth_unstable avoids O(N log K) heap sift-downs on
        // chronologically ordered rowids and keeps 24-byte ISO timestamps inline.
        let mut item_cands: Vec<ItemRankEntry> = Vec::with_capacity(4096);
        let mut event_cands: Vec<EventRankEntry> = Vec::with_capacity(256);
        let mut plan_cands: Vec<(CompactStamp, Box<[u8]>, i64)> = Vec::new();
        let mut asst_by_node: HashMap<
            Box<[u8]>,
            AsstSlot,
            std::hash::BuildHasherDefault<FnvHasher>,
        > = HashMap::with_capacity_and_hasher(4096, Default::default());

        let scan_usable = std::cell::Cell::new(true);
        let mut items_ok = true;
        let mut events_ok = true;
        let mut plans_ok = true;
        let mut asst_ok = true;

        let table = format!("{PREFIX}turn_items");
        let scan_sql = format!(
            "SELECT rowid, thread_id, run_id, node_id, type, updated_at, ordinal
             FROM {table} -- ti_leaf_scan"
        );
        let scan_res = each_row_cached(db, &scan_sql, &[], |row| {
            if !scan_usable.get() {
                return Ok(());
            }
            let typ = match row.get_ref(4)? {
                ValueRef::Text(b) => b,
                ValueRef::Null => return Ok(()),
                _ => {
                    scan_usable.set(false);
                    return Ok(());
                }
            };
            if typ == b"checkpoint" {
                return Ok(());
            }
            let rid = match row.get_ref(0)? {
                ValueRef::Integer(id) => id,
                _ => {
                    scan_usable.set(false);
                    return Ok(());
                }
            };

            if typ == b"todo_list" {
                match row.get_ref(2)? {
                    ValueRef::Text(r) => {
                        if run_id_map.contains_key(r) {
                            let ua = match row.get_ref(5)? {
                                ValueRef::Text(b) => CompactStamp::from_bytes(b),
                                ValueRef::Null => CompactStamp::Null,
                                _ => {
                                    scan_usable.set(false);
                                    return Ok(());
                                }
                            };
                            plan_cands.push((ua, r.to_vec().into_boxed_slice(), rid));
                        }
                    }
                    ValueRef::Null => {}
                    _ => scan_usable.set(false),
                }
                return Ok(());
            }

            let ua = match row.get_ref(5)? {
                ValueRef::Text(b) => Some(b),
                ValueRef::Null => None,
                _ => {
                    scan_usable.set(false);
                    return Ok(());
                }
            };

            // Check the time window before looking up the event's thread.
            let mut cached_tid: Option<Option<&[u8]>> = None;
            let mut get_tid = || -> rusqlite::Result<Option<&[u8]>> {
                if let Some(t) = cached_tid {
                    return Ok(t);
                }
                let t = match row.get_ref(1)? {
                    ValueRef::Text(b) => Some(b),
                    ValueRef::Null => None,
                    _ => {
                        scan_usable.set(false);
                        None
                    }
                };
                cached_tid = Some(t);
                Ok(t)
            };

            if events_ok {
                if let Some(u) = ua {
                    if u >= since_bytes {
                        if let Some(t) = get_tid()? {
                            if active_map.contains_key(t) {
                                event_cands.push(EventRankEntry {
                                    updated_at: CompactStamp::from_bytes(u),
                                    rowid: rid,
                                });
                            }
                        }
                    }
                }
            }

            if is_toolish_bytes(typ) {
                if asst_ok && typ == b"assistant_message" {
                    if rid <= 0 {
                        // Non-positive rowids could collide with t3_asst_hint sentinels 0 / -1.
                        asst_ok = false;
                    } else if let Some(t) = get_tid()? {
                        if let Some(&tid_idx) = active_map.get(t) {
                            match row.get_ref(3)? {
                                ValueRef::Text(nid) => {
                                    let ord = match row.get_ref(6)? {
                                        ValueRef::Integer(o) => Some(Some(o)),
                                        ValueRef::Null => Some(None),
                                        _ => {
                                            asst_ok = false;
                                            None
                                        }
                                    };
                                    if let Some(ord) = ord {
                                        if let Some(slot) = asst_by_node.get_mut(nid) {
                                            match slot {
                                                AsstSlot::One {
                                                    tid,
                                                    ord: o0,
                                                    rowid: r0,
                                                } => {
                                                    let v =
                                                        vec![(*tid, *o0, *r0), (tid_idx, ord, rid)];
                                                    *slot = AsstSlot::Many(v);
                                                }
                                                AsstSlot::Many(v) => v.push((tid_idx, ord, rid)),
                                            }
                                        } else {
                                            asst_by_node.insert(
                                                nid.to_vec().into_boxed_slice(),
                                                AsstSlot::One {
                                                    tid: tid_idx,
                                                    ord,
                                                    rowid: rid,
                                                },
                                            );
                                        }
                                    }
                                }
                                ValueRef::Null => {}
                                _ => asst_ok = false,
                            }
                        }
                    }
                }

                if items_ok {
                    let owner_key = match row.get_ref(2)? {
                        ValueRef::Text(r) => run_id_map.get(r).copied(),
                        ValueRef::Null => {
                            if !runless_set.is_empty()
                                && get_tid()?.is_some_and(|t| runless_set.contains(t))
                            {
                                Some(u32::MAX)
                            } else {
                                None
                            }
                        }
                        _ => {
                            scan_usable.set(false);
                            None
                        }
                    };
                    if let Some(owner_key) = owner_key {
                        let ord = match row.get_ref(6)? {
                            ValueRef::Integer(v) => Some(v),
                            ValueRef::Null => None,
                            _ => {
                                items_ok = false;
                                None
                            }
                        };
                        if items_ok {
                            item_cands.push(ItemRankEntry {
                                updated_at: match ua {
                                    Some(b) => CompactStamp::from_bytes(b),
                                    None => CompactStamp::Null,
                                },
                                ordinal: ord,
                                owner_key,
                                rowid: rid,
                            });
                        }
                    }
                }
            }
            Ok(())
        });

        if scan_res.is_ok() && scan_usable.get() {
            if asst_ok {
                for slot in asst_by_node.values_mut() {
                    if let AsstSlot::Many(vec) = slot {
                        vec.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
                    }
                }
                asst_index = Some(AsstItemIndex {
                    tid_to_idx,
                    by_node: asst_by_node,
                });
            }

            // Finalize ranked winning rowids + tie-break safety checks.
            let mut item_winners: Vec<ItemRankEntry> = if items_ok {
                let cmp = |a: &ItemRankEntry, b: &ItemRankEntry| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| b.ordinal.cmp(&a.ordinal))
                        .then_with(|| a.owner_key.cmp(&b.owner_key))
                        .then_with(|| b.rowid.cmp(&a.rowid))
                };
                if item_cands.len() > 3001 {
                    item_cands.select_nth_unstable_by(3000, cmp);
                    item_cands.truncate(3001);
                }
                item_cands.sort_unstable_by(cmp);
                // Cut-boundary tie: SQLite's index visit order decides which rows make the top 3000.
                if item_cands.len() > 3000
                    && item_cands[2999].updated_at == item_cands[3000].updated_at
                    && item_cands[2999].ordinal == item_cands[3000].ordinal
                {
                    items_ok = false;
                }
                item_cands.truncate(3000);
                // Internal tie only matters when two tied rows share an owner (`run_id`, or both
                // `run_id IS NULL`), since `attach_item` partitions items strictly by `run_id`.
                // Sorting by `(updated_at DESC, ordinal DESC, owner_key ASC)` places any same-owner
                // ties adjacently.
                if items_ok
                    && item_cands.windows(2).any(|w| {
                        w[0].updated_at == w[1].updated_at
                            && w[0].ordinal == w[1].ordinal
                            && w[0].owner_key == w[1].owner_key
                    })
                {
                    items_ok = false;
                }
                if items_ok { item_cands } else { Vec::new() }
            } else {
                Vec::new()
            };

            let mut event_winners: Vec<EventRankEntry> = if events_ok {
                let cmp = |a: &EventRankEntry, b: &EventRankEntry| {
                    b.updated_at
                        .cmp(&a.updated_at)
                        .then_with(|| b.rowid.cmp(&a.rowid))
                };
                if event_cands.len() > 8001 {
                    event_cands.select_nth_unstable_by(8000, cmp);
                    event_cands.truncate(8001);
                }
                event_cands.sort_unstable_by(cmp);
                if event_cands.len() > 8000
                    && event_cands[7999].updated_at == event_cands[8000].updated_at
                {
                    events_ok = false;
                }
                event_cands.truncate(8000);
                if events_ok { event_cands } else { Vec::new() }
            } else {
                Vec::new()
            };

            plan_cands.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
            {
                let mut seen_run_ts: HashSet<(&[u8], Option<&[u8]>)> =
                    HashSet::with_capacity(plan_cands.len());
                for (ua, rid_bytes, _) in &plan_cands {
                    if !seen_run_ts.insert((rid_bytes.as_ref(), ua.as_bytes())) {
                        plans_ok = false;
                        break;
                    }
                }
            }
            if !plans_ok {
                plan_cands.clear();
            }

            // Collect union of winning rowids and fetch in monotonic rowid order.
            let mut union_rids: Vec<i64> =
                Vec::with_capacity(item_winners.len() + event_winners.len() + plan_cands.len());
            for w in &item_winners {
                union_rids.push(w.rowid);
            }
            for w in &event_winners {
                union_rids.push(w.rowid);
            }
            for &(_, _, rid) in &plan_cands {
                union_rids.push(rid);
            }
            union_rids.sort_unstable();
            union_rids.dedup();

            let mut fetched: HashMap<
                i64,
                FetchedTurnItem,
                std::hash::BuildHasherDefault<RowidHasher>,
            > = HashMap::with_capacity_and_hasher(union_rids.len(), Default::default());

            if !union_rids.is_empty() {
                for chunk in union_rids.chunks(900) {
                    let fetch_sql = format!(
                        "SELECT rowid,thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
                         CASE WHEN type<>'todo_list' THEN json_extract(payload_json,'$.startedAt') END AS started_at,
                         CASE WHEN type NOT IN ('secret_request','link_request','todo_list') THEN json_extract(payload_json,'$.toolName') END AS tool,
                         {ITEM_EXIT_CODE},
                         CASE WHEN type IN ('secret_request','link_request','todo_list') THEN ''
                         ELSE substr(COALESCE(json_extract(payload_json,'$.title'),json_extract(payload_json,'$.toolName'),json_extract(payload_json,'$.label'),''),1,240) END AS label,
                         CASE WHEN type='command_execution' THEN substr(json_extract(payload_json,'$.input'),1,240) ELSE '' END AS command,
                         CASE WHEN type='file_change' THEN substr(json_extract(payload_json,'$.fileName'),1,240) ELSE '' END AS file,
                         CASE WHEN type='todo_list' THEN json_extract(payload_json,'$.steps') END AS steps
                         FROM {table} WHERE rowid IN ({}) -- ti_fetch",
                        marks(chunk.len())
                    );
                    let args: Vec<SqlValue> =
                        chunk.iter().copied().map(SqlValue::Integer).collect();
                    each_row_uncached(db, &fetch_sql, &args, |row| {
                        if let ValueRef::Integer(rid) = row.get_ref(0)? {
                            let item = FetchedTurnItem::from_row(row, 1, true)?;
                            fetched.insert(rid, item);
                        }
                        Ok(())
                    })
                    .map_err(|e| db_error(path, e))?;
                }
            }

            if items_ok {
                let mut built = Vec::with_capacity(item_winners.len());
                for w in item_winners.drain(..) {
                    if let Some(row) = fetched.get(&w.rowid) {
                        built.push(row.to_item(path)?);
                    }
                }
                total_rows += built.len();
                items_out = Some(built);
            }
            if events_ok {
                let mut built = Vec::with_capacity(event_winners.len());
                for w in event_winners.drain(..) {
                    if let Some(row) = fetched.get(&w.rowid) {
                        built.push(row.clone());
                    }
                }
                if events_ties_safe(&built) {
                    total_rows += built.len();
                    events_out = Some(built);
                }
            }
            if plans_ok {
                let mut built = Vec::with_capacity(plan_cands.len());
                for (_, _, rid) in plan_cands {
                    if let Some(row) = fetched.get(&rid) {
                        built.push(PlanRow {
                            thread_id: row.thread_id.clone().unwrap_or_default(),
                            run_id: row.run_id.clone(),
                            node_id: row.node_id.clone(),
                            provider_thread_id: row.provider_thread_id.clone(),
                            steps: row.steps.clone(),
                        });
                    }
                }
                total_rows += built.len();
                plans_out = Some(built);
            }
        }
    }

    let run_marks = marks(run_ids.len());
    let items = match items_out {
        Some(v) => v,
        None => {
            let _phase = timing::phase("read_items");
            let mut item_params = strings(run_ids.iter().cloned());
            item_params.extend(strings(runless.iter().cloned()));
            let runless_marks = marks(runless.len());
            let tail = format!(
                "FROM {PREFIX}turn_items WHERE (run_id IN ({run_marks}) OR (run_id IS NULL AND thread_id IN ({runless_marks}))) AND type IN ({TOOLISH}) ORDER BY updated_at DESC,ordinal DESC LIMIT 3000"
            );
            let oracle_sql = format!("SELECT {} {tail}", item_fields());
            let raw = match fetch_by_rowid_two_phase(db, path, &tail, &oracle_sql, &item_params)? {
                Some(rows) => rows,
                None => {
                    let mut rows = Vec::new();
                    each_row_cached(db, &oracle_sql, &item_params, |row| {
                        rows.push(FetchedTurnItem::from_row(row, 0, false)?);
                        Ok(())
                    })
                    .map_err(|e| db_error(path, e))?;
                    rows
                }
            };
            let mut out = Vec::with_capacity(raw.len());
            for r in raw {
                out.push(r.to_item(path)?);
            }
            total_rows += out.len();
            out
        }
    };

    let events = match events_out {
        Some(v) => v,
        None => {
            let _phase = timing::phase("read_events");
            let mut since_params = params.to_vec();
            since_params.push(SqlValue::Text(since.to_owned()));
            let tail = format!(
                "FROM {PREFIX}turn_items {where_ids} AND updated_at>=? AND type NOT IN ('checkpoint','todo_list') ORDER BY updated_at DESC LIMIT 8000"
            );
            let oracle_sql = format!("SELECT {} {tail}", item_fields());
            let out = match fetch_by_rowid_two_phase(db, path, &tail, &oracle_sql, &since_params)? {
                Some(rows) => rows,
                None => {
                    let mut rows = Vec::new();
                    each_row_cached(db, &oracle_sql, &since_params, |row| {
                        rows.push(FetchedTurnItem::from_row(row, 0, false)?);
                        Ok(())
                    })
                    .map_err(|e| db_error(path, e))?;
                    rows
                }
            };
            total_rows += out.len();
            out
        }
    };

    let plans = match plans_out {
        Some(v) => v,
        None => {
            let _phase = timing::phase("read_plans");
            let plans_sql = format!(
                "SELECT thread_id,run_id,node_id,provider_thread_id,updated_at,json_extract(payload_json,'$.steps') AS steps FROM {PREFIX}turn_items WHERE type='todo_list' AND run_id IN ({run_marks}) ORDER BY updated_at DESC"
            );
            let run_params = strings(run_ids.iter().cloned());
            let found = match todo_lists(db, &plans_sql, &run_marks, &run_params) {
                Ok(Some(found)) => found
                    .into_iter()
                    .map(|r| PlanRow {
                        thread_id: str_at(&r, "thread_id").to_owned(),
                        run_id: optional(&r, "run_id"),
                        node_id: optional(&r, "node_id"),
                        provider_thread_id: optional(&r, "provider_thread_id"),
                        steps: optional(&r, "steps"),
                    })
                    .collect(),
                _ => {
                    let mut out = Vec::new();
                    each_row_cached(db, &plans_sql, &run_params, |row| {
                        out.push(PlanRow {
                            thread_id: sql_opt_text(row.get_ref(0)?).unwrap_or_default(),
                            run_id: sql_opt_text(row.get_ref(1)?),
                            node_id: sql_opt_text(row.get_ref(2)?),
                            provider_thread_id: sql_opt_text(row.get_ref(3)?),
                            steps: sql_opt_text(row.get_ref(5)?),
                        });
                        Ok(())
                    })
                    .map_err(|e| db_error(path, e))?;
                    out
                }
            };
            total_rows += found.len();
            found
        }
    };

    Ok((items, events, plans, asst_index, total_rows))
}
