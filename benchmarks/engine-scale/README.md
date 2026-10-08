# Engine scale comparison

An independent Cargo workspace that runs one workload against several embedded
engines at Rocky's target size: 2,000 tables and 500,000 to 5,000,000 rows.
It asks whether the storage engine should change. The answer and the measured
tables are in [RESULTS.md](RESULTS.md). The earlier
[engine comparison](../engine-comparison/DECISION.md) measured concurrent write
throughput on small data and is not replaced by this.

## Layout

- `crates/harness` owns the workload, the timing and the output format.
- `crates/<engine>-engine` implements the `Engine` and `Conn` traits for one
  engine and nothing else.
- `turso-patched/` builds the Turso adapter against a patched engine copy.
- `crates/turso-legacy-probe` is the first Turso-only probe behind
  [engine at target size](RESULTS.md), kept as evidence.
- `run.sh` runs the matrix one process at a time; `results/` holds its output.
- `crates/sqlite-engine/src/bin/sqlite-writer.rs` holds the SQLite batched-writer
  probes; `run-writer.sh` runs them.
- `summarize.py` turns `results/raw/` into the tables in `results/summary.md`.

## Workload

Schema: data tables `t0..tN` with columns `node BLOB, seq INTEGER, a INTEGER,
b INTEGER, c TEXT, lamport INTEGER, stamp_node BLOB, stamp_seq INTEGER`, primary
key `(node, seq)` and one secondary index each on `a` and `b`. Eight domains
each have `ops_D(node, seq, bytes, lamport, status, origin)` with primary key
`(node, seq)`, `outbox_D(node, seq)` and a one-row `replica_D(id, seq, lamport,
revision)`. A key-value engine keeps the same logical layout: one keyspace per
table and per index, and maintains the indexes itself in the same transaction.

`load <dir> <tables> <rows>` installs the schema, loads `rows / tables` rows per
table in transactions of 1,000 rows while a second thread reads and writes,
then measures. `reopen` measures the same store in a fresh process;
`reopen-durable` does so with every commit forced to stable storage. Load and
`reopen` run without flushing.

| Metric | One sample is |
| --- | --- |
| `point_read` | one row by primary key from a random table |
| `index_eq_read` | all rows with one `a` value, about 20 |
| `range_read_100` | 100 rows by a `b` range through the index |
| `insert`, `update` | one committed single-row transaction |
| `mutation` | one committed Rocky-shaped transaction: row upsert, operation, outbox entry, replica row |
| `writers_N_*` | N threads, one domain each, running `mutation` for 3 s |
| `*_bystander_*` | what a second thread saw during the load or a checkpoint |

Results are JSON lines on standard output, appended to `$ENGINE_SCALE_OUT` when set.

## Run

```sh
cargo build --release --locked
(cd turso-patched && ./prepare.sh && cargo build --release --locked)
./run.sh            # full matrix, sequential; about an hour
python3 summarize.py > results/summary.md
ENGINE_SCALE_OUT=out.jsonl ./target/release/sqlite-engine load /tmp/es/sqlite 2000 500000
```

Adapter settings are environment variables read by each adapter; the matrix uses
`TURSO_CACHE_KIB=262144` for the larger Turso cache and `KV_LAYOUT=prefixed` for
fjall. `ENGINE_SCALE_VARIANT` labels the output lines.

## Batched writer probes

`sqlite-writer` measures SQLite alone for
phase 2; results are in
[RESULTS.md](RESULTS.md#sqlite-batched-writer-2026-10-04).

- `sync-switch <dir>` switches `synchronous` between NORMAL and FULL on one
  connection and times commits at each.
- `batched <dir> <files> <domains> <tables-per-domain> <rows-per-table>
  <seconds> <full|normal>` runs one client per domain and one batched writer
  per file. `WRITER_WAL_CHECKPOINT_BYTES` (default 4 MiB), `WRITER_MAX_GROUP`
  (64) and `WRITER_MAX_GROUP_BYTES` (256 KiB) set the writer's limits.

```sh
cargo build --release --locked
./run-writer.sh                            # all sections, 3 repetitions; about 6 minutes
./run-writer.sh results/raw 2 layout-extra # sections: switch batched layout layout-extra one-client
ENGINE_SCALE_OUT=out.jsonl ./target/release/sqlite-writer batched /tmp/esw 1 8 250 250 3 full
```

The root [AGENTS.md](../../AGENTS.md) and the
[benchmark instructions](../engine-comparison/AGENTS.md) apply here too.
