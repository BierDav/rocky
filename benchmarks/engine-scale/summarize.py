#!/usr/bin/env python3
"""Prints one table per size from results/raw/*.jsonl: `summarize.py [dir]`."""
import collections, glob, json, sys

raw = sys.argv[1] if len(sys.argv) > 1 else "results/raw"
data = collections.defaultdict(dict)  # (tables, rows) -> variant -> (mode, metric) -> record
for path in sorted(glob.glob(f"{raw}/*.jsonl")):
    for line in open(path):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        data[(r["tables"], r["rows"])].setdefault(r["variant"], {})[(r["mode"], r["metric"])] = r

def cell(records, mode, metric, field):
    r = records.get((mode, metric))
    if r is None:
        return "–"
    if field == "p50/p99":
        return f'{r["p50_us"]:,} / {r["p99_us"]:,}'
    return f"{r[field]:,}"

ROWS = [
    ("install, ms", "load", "install_ms", "value"),
    ("load, rows/s", "load", "load_rows_per_s", "value"),
    ("load peak resident, MiB", "load", "process", "peak_resident_mib"),
    ("files, MiB", "load", "files_mib", "value"),
    ("checkpoint after load, ms", "load", "load_checkpoint_ms", "value"),
    ("checkpoint, ms", "load", "checkpoint_ms", "value"),
    ("other thread's worst write in checkpoint, µs", "load", "checkpoint_bystander_write", "max_us"),
    ("other thread's worst write in load, µs", "load", "load_bystander_write", "max_us"),
    ("open to first read, µs", "reopen", "open_first_read_us", "value"),
    ("resident after open, MiB", "reopen", "open_resident_mib", "value"),
    ("reopen peak resident, MiB", "reopen", "process", "peak_resident_mib"),
    ("point read, µs p50/p99", "reopen", "point_read", "p50/p99"),
    ("index equality read, µs", "reopen", "index_eq_read", "p50/p99"),
    ("range read 100, µs", "reopen", "range_read_100", "p50/p99"),
    ("insert, µs", "reopen", "insert", "p50/p99"),
    ("update, µs", "reopen", "update", "p50/p99"),
    ("mutation, µs", "reopen", "mutation", "p50/p99"),
    ("1 writer, tx/s", "reopen", "writers_1_tx_per_s", "value"),
    ("4 writers, tx/s", "reopen", "writers_4_tx_per_s", "value"),
    ("8 writers, tx/s", "reopen", "writers_8_tx_per_s", "value"),
    ("8 writers, worst commit, µs", "reopen", "writers_8_mutation", "max_us"),
    ("read beside 4 writers, µs", "reopen", "writers_4_with_reader_point_read", "p50/p99"),
    ("durable mutation, µs", "reopen-durable", "mutation", "p50/p99"),
    ("durable 1 writer, tx/s", "reopen-durable", "writers_1_tx_per_s", "value"),
    ("durable 4 writers, tx/s", "reopen-durable", "writers_4_tx_per_s", "value"),
    ("durable 8 writers, tx/s", "reopen-durable", "writers_8_tx_per_s", "value"),
    ("durable 8 writers, worst commit, µs", "reopen-durable", "writers_8_mutation", "max_us"),
]
for (tables, rows), variants in sorted(data.items()):
    names = sorted(variants)
    print(f"\n### {tables:,} tables, {rows:,} rows\n")
    print("| | " + " | ".join(names) + " |")
    print("| --- |" + " ---: |" * len(names))
    for label, mode, metric, field in ROWS:
        print(f"| {label} | " + " | ".join(cell(variants[n], mode, metric, field) for n in names) + " |")
    failed = [f'{n} {m}: exit {r["exit"]}' for n in names for (m, k), r in variants[n].items() if k == "process" and r["exit"]]
    if failed:
        print("\nFailed: " + "; ".join(failed))
