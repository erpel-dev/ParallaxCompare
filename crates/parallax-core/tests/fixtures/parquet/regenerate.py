#!/usr/bin/env python3
"""Builds the Parquet and Arrow files the tests read (checked in, so tests need no Python).

  sales-v1.parquet         snappy (the pyarrow default), with pandas metadata, nested list and struct columns
  sales-v2.parquet         v1 with one changed value and a new column
  sales-v1-zstd.parquet    v1 compressed with zstd
  sales-v1-groups.parquet  v1 in row groups of 2 rows, uncompressed, no statistics for "note"
  sales-v1.feather         v1 as Arrow IPC (Feather v2), lz4
  sales-v2.arrow           v2 as an uncompressed Arrow IPC file in record batches of 3 rows
"""
import datetime
import decimal
import os

import pyarrow as pa
import pyarrow.feather as feather
import pyarrow.parquet as pq

here = os.path.dirname(os.path.abspath(__file__))
out = lambda name: os.path.join(here, name)


def table(version):
    rows = {
        "id": pa.array([1, 2, 3, 4, 5], pa.int64()),
        "product": pa.array(["apple", "Birne", "cherry", None, "Zitrone 🍋"]),
        "price": pa.array([decimal.Decimal("1.25"), decimal.Decimal("0.99"), decimal.Decimal("12.50"), None,
                           decimal.Decimal("3.00")], pa.decimal128(9, 2)),
        "qty": pa.array([10, 0, 3, 7, 1], pa.int32()),
        "ratio": pa.array([0.5, 1.0, None, 2.25, -1.5], pa.float64()),
        "sold": pa.array([datetime.date(2024, 1, d) for d in (1, 2, 3, 4, 5)], pa.date32()),
        "at": pa.array([datetime.datetime(2024, 1, 1, 12, 0, s) for s in range(5)], pa.timestamp("us", tz="UTC")),
        "ok": pa.array([True, False, True, None, True]),
        "tags": pa.array([["a", "b"], [], None, ["c"], ["d", "e", "f"]], pa.list_(pa.string())),
        "dims": pa.array([{"w": 1, "h": 2}, {"w": 3, "h": 4}, None, {"w": 5, "h": 6}, {"w": 7, "h": 8}],
                         pa.struct([("w", pa.int16()), ("h", pa.int16())])),
        "note": pa.array(["x" * 10, None, "y", "z", None]),
    }
    if version == 2:
        rows["qty"] = pa.array([10, 0, 3, 7, 99], pa.int32())
        rows["region"] = pa.array(["north", "south", "east", "west", "north"]).dictionary_encode()
    t = pa.table(rows)
    return t.replace_schema_metadata({**(t.schema.metadata or {}), b"source": b"regenerate.py", b"version": str(version).encode()})


v1, v2 = table(1), table(2)
pq.write_table(v1, out("sales-v1.parquet"))
pq.write_table(v2, out("sales-v2.parquet"))
pq.write_table(v1, out("sales-v1-zstd.parquet"), compression="zstd")
pq.write_table(v1, out("sales-v1-groups.parquet"), row_group_size=2, compression="none",
               write_statistics=[c for c in v1.column_names if c != "note"])
feather.write_feather(v1, out("sales-v1.feather"))
with pa.ipc.new_file(out("sales-v2.arrow"), v2.schema) as w:
    for batch in v2.to_batches(max_chunksize=3):
        w.write_batch(batch)
