//! Witnesses for the projected turn-item rows of one run in a pinned WAL snapshot.
use super::wal::WalView;
use super::{Key, digest};
use rusqlite::{Connection, StatementStatus, params};
use std::collections::HashSet;

const PROBE: &str = "SELECT ordinal,rowid,sqlite_offset(type)
    FROM main.orchestration_v2_projection_turn_items
    INDEXED BY orchestration_v2_projection_turn_items_run_ordinal_idx
    WHERE run_id=?1 ORDER BY ordinal,rowid LIMIT ?2";
const MAX_ITEMS: usize = 16_384;
const MAX_POINTER_BYTES: usize = 4 * 1024 * 1024;
const MAX_OVERFLOW_PAGES: usize = 16_384;

#[derive(Hash)]
struct Item {
    ordinal: i64,
    rowid: i64,
    leaf: u32,
    record: usize,
}

fn probe(db: &Connection, wal: &WalView<'_>, run_id: &str) -> Option<Vec<Item>> {
    let mut stmt = db.prepare_cached(PROBE).ok()?;
    stmt.reset_status(StatementStatus::FullscanStep);
    stmt.reset_status(StatementStatus::Sort);
    let mut rows = stmt.query(params![run_id, (MAX_ITEMS + 1) as i64]).ok()?;
    let mut items: Vec<Item> = Vec::new();
    let page_size = u64::from(wal.page_size());
    while let Some(row) = rows.next().ok()? {
        if items.len() == MAX_ITEMS {
            return None;
        }
        let ordinal = row.get(0).ok()?;
        let rowid = row.get(1).ok()?;
        let offset = u64::try_from(row.get::<_, i64>(2).ok()?).ok()?;
        // Bundled sqlite3.c: OP_Offset and sqlite3BtreeOffset return the record's
        // logical database offset, even when the leaf is currently in the WAL.
        // `type` is outside the T3 index, so OP_Offset uses the table cursor.
        let leaf = u32::try_from(offset / page_size + 1).ok()?;
        let record = (offset % page_size) as usize;
        if leaf > wal.page_count()
            || leaf == pending_page(wal)
            || record >= wal.usable_size() as usize
            || items
                .last()
                .is_some_and(|p| (p.ordinal, p.rowid) >= (ordinal, rowid))
        {
            return None;
        }
        items.push(Item {
            ordinal,
            rowid,
            leaf,
            record,
        });
    }
    drop(rows);
    (stmt.get_status(StatementStatus::FullscanStep) == 0
        && stmt.get_status(StatementStatus::Sort) == 0)
        .then_some(items)
}

fn pending_page(wal: &WalView<'_>) -> u32 {
    // Bundled sqlite3.c: PENDING_BYTE and PENDING_BYTE_PAGE. SQLite never
    // allocates this page to a table or as an overflow page.
    0x4000_0000 / wal.page_size() + 1
}

fn valid_overflow_page(page: u32, wal: &WalView<'_>) -> bool {
    page > 1 && page <= wal.page_count() && page != pending_page(wal)
}

fn leaf_cells(
    page: u32,
    usable: usize,
    pointer_budget: &mut usize,
    read: &mut impl FnMut(usize, &mut [u8]) -> Option<()>,
) -> Option<Vec<usize>> {
    // Bundled sqlite3.c: btreeInitPage and btreeComputeFreeSpace. Page 1
    // reserves the first 100 bytes for the database header.
    let start = if page == 1 { 100 } else { 0 };
    let mut header = [0; 8];
    read(start, &mut header)?;
    if header[0] != 0x0d || header[7] > 60 {
        return None;
    }
    let freeblock = usize::from(u16::from_be_bytes([header[1], header[2]]));
    let count = usize::from(u16::from_be_bytes([header[3], header[4]]));
    let content = match u16::from_be_bytes([header[5], header[6]]) {
        0 => 65_536,
        offset => usize::from(offset),
    };
    let pointers = start + header.len();
    let pointer_bytes = count * 2;
    if count == 0
        || pointers + pointer_bytes > content
        || count * 4 > usable.checked_sub(content)?
        || (freeblock != 0 && !(content..=usable - 4).contains(&freeblock))
    {
        return None;
    }
    *pointer_budget = pointer_budget.checked_sub(pointer_bytes)?;
    let mut bytes = vec![0; pointer_bytes];
    read(pointers, &mut bytes)?;
    let mut cells: Vec<_> = bytes
        .chunks_exact(2)
        .map(|p| usize::from(u16::from_be_bytes([p[0], p[1]])))
        .collect();
    cells.sort_unstable();
    if cells[0] < content
        || *cells.last()? > usable - 4
        || cells.windows(2).any(|pair| pair[0] + 4 > pair[1])
    {
        return None;
    }
    Some(cells)
}

fn varint(
    at: &mut usize,
    end: usize,
    read: &mut impl FnMut(usize, &mut [u8]) -> Option<()>,
) -> Option<u64> {
    let mut value = 0u64;
    // Bundled sqlite3.c: sqlite3GetVarint. The ninth byte uses all eight bits.
    // Read one byte at a time to stop before the first byte of the record.
    for i in 0..9 {
        if *at >= end {
            return None;
        }
        let mut byte = [0];
        read(*at, &mut byte)?;
        *at += 1;
        if i == 8 {
            return Some((value << 8) | u64::from(byte[0]));
        }
        value = (value << 7) | u64::from(byte[0] & 0x7f);
        if byte[0] < 0x80 {
            return Some(value);
        }
    }
    unreachable!("the ninth byte terminates a SQLite varint")
}

#[derive(Debug, PartialEq, Eq)]
struct Overflow {
    first: u32,
    pages: usize,
}

fn cell_overflow(
    cells: &[usize],
    item: &Item,
    usable: usize,
    read: &mut impl FnMut(usize, &mut [u8]) -> Option<()>,
) -> Option<Option<Overflow>> {
    // A well-formed leaf holds non-overlapping cells. The last pointer before
    // the reported record is its only possible start. Verify both header varints.
    let next = cells.partition_point(|&cell| cell < item.record);
    let mut at = *cells.get(next.checked_sub(1)?)?;
    if item.record - at > 18 {
        return None;
    }
    let payload = varint(&mut at, item.record, read)?;
    if payload == 0 || payload > i32::MAX as u64 {
        return None;
    }
    let payload = payload as usize;
    let rowid = varint(&mut at, item.record, read)? as i64;
    if at != item.record || rowid != item.rowid {
        return None;
    }
    let end = cells.get(next).copied().unwrap_or(usable);
    // Bundled sqlite3.c: btreeParseCellPtr and btreeParseCellAdjustSizeForOverflow.
    // The size limits are for table leaves and use only usable bytes (see maxLeaf/minLeaf).
    let max_local = usable - 35;
    if payload <= max_local {
        return (item.record + payload <= end).then_some(None);
    }
    let min_local = (usable - 12) * 32 / 255 - 23;
    let capacity = usable - 4;
    let surplus = min_local + (payload - min_local) % capacity;
    let local = if surplus <= max_local {
        surplus
    } else {
        min_local
    };
    let link = item.record + local;
    if link + 4 > end {
        return None;
    }
    let mut first = [0; 4];
    read(link, &mut first)?;
    Some(Some(Overflow {
        first: u32::from_be_bytes(first),
        pages: (payload - local).div_ceil(capacity),
    }))
}

impl Overflow {
    fn collect(
        self,
        seen: &mut HashSet<u32>,
        mut read_link: impl FnMut(u32) -> Option<u32>,
    ) -> Option<()> {
        if self.pages > MAX_OVERFLOW_PAGES.checked_sub(seen.len())? {
            return None;
        }
        let mut page = self.first;
        for _ in 0..self.pages {
            if page < 2 || !seen.insert(page) {
                return None;
            }
            page = read_link(page)?;
        }
        (page == 0).then_some(())
    }
}

pub(super) fn capture(db: &Connection, wal: &WalView<'_>, run_id: &str) -> Option<Vec<u8>> {
    let mut items = probe(db, wal, run_id)?;
    let members = digest("wal item locations", &items);
    // Group requests by physical leaf so its pointer array is read once per run.
    items.sort_unstable_by_key(|item| item.leaf);
    let mut overflow = HashSet::new();
    let mut pointer_budget = MAX_POINTER_BYTES;
    let usable = wal.usable_size() as usize;
    for group in items.chunk_by(|a, b| a.leaf == b.leaf) {
        let leaf = group[0].leaf;
        let mut read = |offset, out: &mut [u8]| wal.read_part(leaf, offset, out);
        let cells = leaf_cells(leaf, usable, &mut pointer_budget, &mut read)?;
        for item in group {
            if let Some(chain) = cell_overflow(&cells, item, usable, &mut read)? {
                chain.collect(&mut overflow, |page| {
                    if !valid_overflow_page(page, wal)
                        || items.binary_search_by_key(&page, |item| item.leaf).is_ok()
                    {
                        return None;
                    }
                    let mut next = [0; 4];
                    wal.read_part(page, 0, &mut next)?;
                    Some(u32::from_be_bytes(next))
                })?;
            }
        }
    }
    let mut pages: Vec<_> = overflow.into_iter().collect();
    pages.sort_unstable();
    let mut encoded = Vec::with_capacity(members.len() + pages.len() * 4);
    encoded.extend_from_slice(&members);
    encoded.extend(pages.into_iter().flat_map(u32::to_be_bytes));
    Some(encoded)
}

pub(super) fn unchanged(
    db: &Connection,
    wal: &WalView<'_>,
    run_id: &str,
    encoded: &[u8],
    changed: &HashSet<u32>,
) -> bool {
    let Some((members, overflow)) = encoded.split_first_chunk::<{ size_of::<Key>() }>() else {
        return false;
    };
    let (pages, tail) = overflow.as_chunks::<4>();
    if pages.len() > MAX_OVERFLOW_PAGES || !tail.is_empty() {
        return false;
    }
    let mut previous = 1;
    for &bytes in pages {
        let page = u32::from_be_bytes(bytes);
        if page <= previous || !valid_overflow_page(page, wal) || changed.contains(&page) {
            return false;
        }
        previous = page;
    }
    // A merge can free an old leaf without writing a frame for it. Probe the
    // current locations so a row moved off that leaf cannot look unchanged.
    probe(db, wal, run_id).is_some_and(|items| {
        !items.iter().any(|item| changed.contains(&item.leaf))
            && digest("wal item locations", &items) == *members
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_searches_the_t3_run_index() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!(
            "../../../../checks/fixtures/t3-schema-main-096.sql"
        ))
        .unwrap();
        let args = params!["synthetic", (MAX_ITEMS + 1) as i64];
        let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {PROBE}")).unwrap();
        let details: Vec<String> = plan
            .query_map(args, |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        eprintln!("T3 schema 096 item witness plan: {}", details.join("; "));
        assert_eq!(
            details,
            [
                "SEARCH main.orchestration_v2_projection_turn_items USING INDEX orchestration_v2_projection_turn_items_run_ordinal_idx (run_id=?)"
            ]
        );

        let mut explain = db.prepare(&format!("EXPLAIN {PROBE}")).unwrap();
        let ops: Vec<(String, i64, i64)> = explain
            .query_map(args, |row| Ok((row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let offset = ops.iter().find(|op| op.0 == "Offset").unwrap();
        assert!(
            ops.iter()
                .any(|op| op.0 == "Column" && op.1 != offset.1 && op.2 == 1)
        );
        assert!(!ops.iter().any(|op| op.0 == "Column" && op.1 == offset.1));
    }

    #[test]
    fn leaf_at_64k_boundary_reads_only_header_and_pointers() {
        let item = Item {
            ordinal: 1,
            rowid: -1,
            leaf: 1,
            record: 65_532,
        };
        let mut page = vec![0; 65_536];
        let start = item.record - 10;
        let pointer = (start as u16).to_be_bytes();
        page[100..108].copy_from_slice(&[0x0d, 0, 0, 0, 1, pointer[0], pointer[1], 0]);
        page[108..110].copy_from_slice(&pointer);
        page[start] = 4;
        page[start + 1..item.record].fill(0xff);
        let mut reads = Vec::new();
        let mut read = |at: usize, out: &mut [u8]| {
            let end = at + out.len();
            reads.push(at..end);
            out.copy_from_slice(&page[at..end]);
            Some(())
        };
        let mut budget = MAX_POINTER_BYTES;
        let cells = leaf_cells(1, page.len(), &mut budget, &mut read).unwrap();
        assert_eq!(
            cell_overflow(&cells, &item, page.len(), &mut read),
            Some(None)
        );
        let expected: Vec<_> = [100..108, 108..110]
            .into_iter()
            .chain((start..item.record).map(|at| at..at + 1))
            .collect();
        assert_eq!(reads, expected);
    }

    #[test]
    fn overflow_uses_usable_size_and_reads_only_the_link() {
        let item = Item {
            ordinal: 1,
            rowid: 1,
            leaf: 2,
            record: 412,
        };
        let mut read = |at, out: &mut [u8]| {
            match at {
                409 => out.copy_from_slice(&[0x87]),
                410 => out.copy_from_slice(&[0x68]),
                411 => out.copy_from_slice(&[1]),
                460 => out.copy_from_slice(&3u32.to_be_bytes()),
                _ => panic!("read outside cell metadata at {at}"),
            }
            Some(())
        };
        let result = cell_overflow(&[409], &item, 480, &mut read);
        assert_eq!(result, Some(Some(Overflow { first: 3, pages: 2 })));
        assert_eq!(cell_overflow(&[409, 462], &item, 480, &mut read), None);
        assert_eq!(
            cell_overflow(&[409], &Item { rowid: 2, ..item }, 480, &mut read),
            None
        );
    }

    #[test]
    fn rejects_malformed_leaf_and_truncated_cell_headers() {
        let header = [0x0d, 0, 0, 0, 2, 1, 0, 0];
        let mut read = |at, out: &mut [u8]| {
            match at {
                0 => out.copy_from_slice(&header),
                // The same cell pointer appears twice.
                8 => out.copy_from_slice(&[1, 0, 1, 0]),
                _ => panic!("read beyond header and cell pointer array"),
            }
            Some(())
        };
        let mut budget = 4;
        assert!(leaf_cells(2, 512, &mut budget, &mut read).is_none());

        let item = Item {
            ordinal: 1,
            rowid: 1,
            leaf: 2,
            record: 258,
        };
        let mut read = |at, out: &mut [u8]| {
            assert!(at < item.record);
            out.copy_from_slice(&[0x80]);
            Some(())
        };
        assert!(cell_overflow(&[256], &item, 512, &mut read).is_none());
    }

    #[test]
    fn overflow_chain_requires_unique_pages_and_exact_termination() {
        let mut seen = HashSet::new();
        let chain = || Overflow { first: 3, pages: 2 };
        assert_eq!(
            chain().collect(&mut seen, |n| Some(if n == 3 { 4 } else { 0 })),
            Some(())
        );
        assert_eq!(seen, HashSet::from([3, 4]));
        assert_eq!(
            chain().collect(&mut seen, |_| panic!("read a repeated overflow page")),
            None
        );
        assert_eq!(chain().collect(&mut HashSet::new(), |n| Some(n)), None);
        assert_eq!(chain().collect(&mut HashSet::new(), |_| Some(0)), None);
        assert_eq!(chain().collect(&mut HashSet::new(), |n| Some(n + 1)), None);
        let over_budget = Overflow {
            first: 3,
            pages: MAX_OVERFLOW_PAGES + 1,
        };
        assert_eq!(
            over_budget.collect(&mut HashSet::new(), |_| panic!("read beyond page budget")),
            None
        );
    }
}
