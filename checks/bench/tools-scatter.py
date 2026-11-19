#!/usr/bin/env python3
"""Measure how scattered a tool row's payload is on disk, without dbstat.

For a seeded sample of rows of `orchestration_v2_projection_turn_items`, walk
the table b-tree to the row's leaf cell, read its payload's local part and
follow the overflow chain page by page.  An overflow page's first four bytes
are the next page number (SQLite file format).  The row's pages are the leaf
page plus its overflow pages, in chain order.

For each file the report gives, over the sample: how many rows have all their
pages adjacent, how many consecutive page pairs are adjacent, the distance
between consecutive pages (median, p90, largest), and the file's free-page and
page counts.  It reads the file directly and never writes; all databases are
synthetic ones built by the lab.

    python3 checks/bench/tools-scatter.py --db checks/generated/tools-lab/week.sqlite \
      --db checks/generated/tools-lab/week-aged.sqlite --rows 400 --seed 20261118

The read is a page walk by offset; `--out` saves the report as JSON.
"""

import argparse
import json
import os
from pathlib import Path
import random
import sqlite3
import statistics
import sys

TOOL_TYPES = "('command_execution','dynamic_tool','file_change','file_search','web_search')"
TABLE = "orchestration_v2_projection_turn_items"


def varint(buf, offset):
    value = 0
    for index in range(9):
        byte = buf[offset + index]
        if index == 8:
            return (value << 8) | byte, offset + 9
        value = (value << 7) | (byte & 0x7F)
        if not byte & 0x80:
            return value, offset + index + 1
    raise ValueError("varint exceeded nine bytes")


class File:
    """Just enough of the SQLite file format to walk a table row's pages."""

    def __init__(self, path):
        self.path = path
        self.fd = os.open(path, os.O_RDONLY)
        header = os.pread(self.fd, 100, 0)
        self.page_size = int.from_bytes(header[16:18], "big")
        if self.page_size == 1:
            self.page_size = 65536
        self.reserved = header[20]
        self.usable = self.page_size - self.reserved
        self.max_local = self.usable - 35
        self.min_local = ((self.usable - 12) * 32) // 255 - 23
        self.pages = os.fstat(self.fd).st_size // self.page_size

    def close(self):
        os.close(self.fd)

    def page(self, number):
        data = os.pread(self.fd, self.page_size, (number - 1) * self.page_size)
        if len(data) != self.page_size:
            raise ValueError(f"page {number} is short: {len(data)}")
        return data

    def find_cell(self, root, rowid):
        """Return (leaf page number, cell bytes) for a table row, or None."""
        page_number = root
        for _ in range(64):
            page = self.page(page_number)
            kind = page[0]
            cells = int.from_bytes(page[3:5], "big")
            start = 12 if kind == 0x05 else 8  # interior headers carry a right-most pointer
            pointers = [int.from_bytes(page[start + 2 * i:start + 2 * i + 2], "big") for i in range(cells)]
            if kind == 0x0D:  # leaf table page
                for pointer in pointers:
                    _, after = varint(page, pointer)
                    found, _ = varint(page, after)
                    if found == rowid:
                        return page_number, page[pointer:]
                return None
            if kind != 0x05:
                raise ValueError(f"page {page_number} is not a table page (type {kind:#x})")
            rightmost = int.from_bytes(page[8:12], "big")
            child = rightmost
            for pointer in pointers:
                key, _ = varint(page, pointer + 4)
                if rowid <= key:
                    child = int.from_bytes(page[pointer:pointer + 4], "big")
                    break
            page_number = child
        raise ValueError("b-tree deeper than 64 pages")

    def row_pages(self, root, rowid):
        """The leaf page and every overflow page of one row, in chain order."""
        leaf, cell = self.find_cell(root, rowid)
        if leaf is None:
            return None
        length, offset = varint(cell, 0)
        found, offset = varint(cell, offset)
        if found != rowid:
            raise ValueError(f"cell rowid {found} is not {rowid}")
        if length <= self.max_local:
            local = length
        else:
            local = self.min_local + (length - self.min_local) % (self.usable - 4)
            if local > self.max_local:
                local = self.min_local
        offset += local
        pages = [leaf]
        if local < length:
            next_page = int.from_bytes(cell[offset:offset + 4], "big")
            while next_page:
                pages.append(next_page)
                overflow = self.page(next_page)
                next_page = int.from_bytes(overflow[0:4], "big")
        carried = local + (self.usable - 4) * (len(pages) - 1)
        if carried < length:
            raise ValueError(f"row {rowid}: overflow chain carries {carried} of {length} bytes")
        return {"rowid": rowid, "bytes": length, "pages": pages}


def describe(path, rows, seed):
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    root = connection.execute("SELECT rootpage FROM sqlite_schema WHERE type='table' AND name=?",
                              (TABLE,)).fetchone()[0]
    # Sample by turn_item_id, not rowid: the two files store the same items in a
    # different order, so the same sample must be looked up by its own rowid.
    items = {item: (rowid, length) for rowid, item, length in connection.execute(
        f"SELECT rowid, turn_item_id, length(payload_json) FROM {TABLE} WHERE type IN {TOOL_TYPES}")}
    try:
        tree = dict(connection.execute("SELECT pageno, pagetype FROM dbstat WHERE name=?", (TABLE,)))
    except sqlite3.Error:
        tree = None
    free = connection.execute("PRAGMA freelist_count").fetchone()[0]
    page_count = connection.execute("PRAGMA page_count").fetchone()[0]
    page_size = connection.execute("PRAGMA page_size").fetchone()[0]
    connection.close()

    chosen = sorted(random.Random(seed).sample(sorted(items), min(rows, len(items))))
    store = File(path)
    try:
        walked = [store.row_pages(root, items[item][0]) for item in chosen]
    finally:
        store.close()
    for index, row in enumerate(walked):
        # The b-tree cell carries the whole record, not just payload_json, so it
        # must be at least that column.  dbstat then says the pages belong to
        # this table and to the kind of page they should be.
        if row is None or row["bytes"] < items[chosen[index]][1]:
            raise RuntimeError(f"{path.name}: page walk disagrees with SQL for row {row['rowid']}")
        if tree is not None:
            kinds = [tree.get(page) for page in row["pages"]]
            if kinds[0] != "leaf" or any(kind != "overflow" for kind in kinds[1:]):
                raise RuntimeError(f"{path.name}: row {row['rowid']} walked pages {kinds}")

    distances, adjacent_pairs, backwards_pairs, pairs, pages, contiguous = [], 0, 0, 0, 0, 0
    per_row = []
    for row in walked:
        chain = row["pages"]
        pages += len(chain)
        steps = [b - a for a, b in zip(chain, chain[1:])]
        steps_adjacent = sum(1 for step in steps if step == 1)
        adjacent_pairs += steps_adjacent
        backwards_pairs += sum(1 for step in steps if step < 0)
        pairs += len(steps)
        distances += steps
        contiguous += int(bool(steps) and steps_adjacent == len(steps))
        per_row.append({"item": chosen[len(per_row)], "rowid": row["rowid"], "bytes": row["bytes"], "pages": len(chain),
                        "adjacent": steps_adjacent, "distance": steps[:40]})
    return {
        "database": str(path), "bytes": path.stat().st_size,
        "page_size": page_size, "page_count": page_count, "free_pages": free,
        "free_fraction": free / page_count if page_count else 0.0,
        "rows_total": len(items), "rows_sampled": len(walked), "seed": seed,
        "dbstat_checked": tree is not None, "table_pages": len(tree) if tree is not None else None,
        "pages_sampled": pages, "row_pages": pages / len(walked) if walked else 0.0,
        "rows_all_pages_adjacent": contiguous,
        "adjacent_pairs": adjacent_pairs, "pairs": pairs,
        "adjacent_fraction": adjacent_pairs / pairs if pairs else 0.0,
        "backwards_pairs": backwards_pairs,
        "distance_median": statistics.median(distances) if distances else 0,
        "distance_p90": sorted(distances)[max(0, -(-9 * len(distances) // 10) - 1)] if distances else 0,
        "distance_largest": max(distances) if distances else 0,
        "distance_mean": statistics.fmean(distances) if distances else 0.0,
        "sample": per_row,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", action="append", required=True, help="a synthetic lab database; repeat to compare")
    parser.add_argument("--rows", type=int, default=400, help="rows to sample per database (at least 100)")
    parser.add_argument("--seed", type=int, default=20261118)
    parser.add_argument("--out", help="optional JSON report path")
    args = parser.parse_args()
    if args.rows < 100:
        parser.error("--rows must be at least 100")
    report = []
    for entry in args.db:
        path = Path(entry).expanduser().resolve()
        if not path.is_file():
            parser.error(f"missing lab: {path}")
        wal = Path(str(path) + "-wal")
        if wal.is_file() and wal.stat().st_size:
            print(f"warning: {wal.name} has {wal.stat().st_size} bytes; the main file may not be current", flush=True)
        result = describe(path, args.rows, args.seed)
        report.append(result)
        print(f"{path.name}: {result['bytes']} bytes, {result['page_count']} pages of {result['page_size']}, "
              f"{result['free_pages']} free ({result['free_fraction'] * 100:.2f}%); "
              f"{result['rows_sampled']} of {result['rows_total']} tool rows sampled, "
              f"{result['pages_sampled']} pages ({result['row_pages']:.2f} per row)", flush=True)
        print(f"  all pages adjacent: {result['rows_all_pages_adjacent']}/{result['rows_sampled']}; "
              f"adjacent consecutive pairs: {result['adjacent_pairs']}/{result['pairs']} "
              f"({result['adjacent_fraction'] * 100:.1f}%); backwards pairs: {result['backwards_pairs']}", flush=True)
        print(f"  distance between consecutive pages: median {result['distance_median']}, "
              f"p90 {result['distance_p90']}, mean {result['distance_mean']:.1f}, "
              f"largest {result['distance_largest']}", flush=True)
    if args.out:
        output = Path(args.out).expanduser().resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps({"seed": args.seed, "rows_requested": args.rows, "databases": report},
                                     indent=2) + "\n")
        print(f"report {output}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, sqlite3.Error, OSError) as error:
        sys.exit(str(error))
