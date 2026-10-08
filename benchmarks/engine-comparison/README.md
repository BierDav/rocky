# Rocky DB engine comparison

This directory is an independent Rust workspace. Existing architecture proposals are unchanged. Read [CRITERIA.md](CRITERIA.md), [DECISION.md](DECISION.md), [ASSESSMENT.md](ASSESSMENT.md), and [results/MEASUREMENTS.md](results/MEASUREMENTS.md).

## Reproduce

On macOS ARM64 with Python 3 (the recorded runs used Rust 1.95.0; the repository
now pins Rust 1.99.0, so compiler-dependent timings are not directly comparable):

```sh
cd benchmarks/engine-comparison
cargo build --release --locked
./target/release/rocky-engine-bench check
python3 crash.py
python3 run.py                         # 270 short disk cases + 9 crypto cases
python3 focused.py                     # 108 longer/decoded/observer/memory/sync cases
python3 consumer_followup.py            # 18 reserved-consumer cases
python3 storage_followup.py             # 18 matched-handle storage cases
python3 lag_followup.py                 # 18 commit-to-consumption lag cases
python3 logical_followup.py             # 18 complete JSON operation controls
python3 crypto_followup.py              # 9 complete operation crypto controls
python3 -m venv .venv
.venv/bin/pip install -r plotting-requirements.txt
.venv/bin/python analyze.py
```

The final source encodes complete declared JSON operations (entity IDs, deltas, tags, author and authority job/dedup IDs), and persists the crypto signature, nonce and ciphertext. Initial profiles used compact symbolic operation markers; `logical-followup.jsonl` checks actual operation-payload overhead and `crypto-logical.jsonl` checks equivalent crypto with complete payloads. The final source prepares producer capture connections before launching consumers, avoiding an initialization lock race. The final source retains an active SQLite WAL connection and reserves the WAL consumer writer up front. Historical decoded-image cases used [evidence/focused-main.rs.txt](evidence/focused-main.rs.txt) before those consumer/lifecycle changes. The recorded initial matrix used [evidence/matrix-main.rs.txt](evidence/matrix-main.rs.txt); the final adapter adds typed CDC decoding, effective PRAGMA recording, detailed retry categories, capture byte counts and revision-based observers. To reproduce that historical matrix exactly, temporarily copy the snapshot to src/main.rs and rebuild, then restore the final source. The final source runs all scripts with improved instrumentation and full operation bodies. Retained-handle/lag historical source is also saved in evidence/retained-handle-main.rs.txt and evidence/lag-main.rs.txt. Tests were strengthened between profiles; those additions do not change timed write behavior. Do not mix profiles when aggregating. Cargo.lock pins transitive dependencies; all direct crate versions are exact. `run.py --quick --output results-new` runs two cases, three times. Scripts append raw results: use an empty output directory for a fresh experiment. `/usr/bin/time -l` is macOS-specific; port process-resource collection for Linux.

Single case (fresh path required):

```sh
./target/release/rocky-engine-bench run mvcc cdc independent 4 1 100 1 /tmp/rocky-new
./target/release/rocky-engine-bench run sqlite trigger independent 4 1 100 1 /tmp/rocky-sqlite-new
./target/release/rocky-engine-bench run wal explicit draft 1 4 100 1 /tmp/rocky-crypto-new crypto
ROCKY_SYNC=NORMAL ./target/release/rocky-engine-bench run mvcc cdc independent 4 1 100 1 /tmp/rocky-weaker
./target/release/rocky-engine-bench run sqlite trigger draft 1 1 100 1 memory
```

Engine names: `sqlite` = bundled upstream SQLite; `wal` and `mvcc` = Rust Turso 0.8.1. Capture modes: `none`, `trigger` (SQLite only), `cdc` (Turso), `explicit` (either). Memory tests are single writer; SQLite shared memory cannot provide WAL persistence and reports its actual journal mode. NORMAL results are weaker durability and never enter the main FULL comparison.

## Adapter contract

Every managed captured transaction inserts `txmeta` (logical identity, origin, declared mutation count, logical payload) atomically with projection writes. This is framework metadata, not something CDC can infer. SQLite generated INSERT/UPDATE/DELETE triggers stage old/new JSON for seven tables; a transaction-scoped context row supplies origin and ID. The one-writer-per-file restriction makes that row safe. Turso enables full CDC on every producer connection, after journal initialization; its CDC v2 records supply transaction grouping and commit boundaries. The consumer joins the captured txmeta marker to the CDC transaction ID, filters business tables and local origin, reads the images, and commits a consumed-ID record plus a cursor transaction count. Consumer connections have CDC disabled.

The explicit control records only declared logical operations. Capture-disabled runs omit txmeta and staging/CDC writes. Logical mutations count requested ERP operations, not physical rows: a draft operation updates invoice, line, tag and author cursor (four rows). Both captured row adapters cover those rows; the explicit adapter records intent. Replication is marked remote and updates the same projections without publication. Turso still captures remote row images; origin filtering suppresses reauthoring. SQLite suppresses them in triggers. This is a real cost difference.

The durable cursor is a **set of consumed transaction identities**, with a transaction count for progress; it is intentionally not a scalar maximum change ID. Every drain scans retained committed transaction markers and anti-joins consumed IDs in one snapshot, then atomically advances progress. Delayed concurrent commits cannot disappear below a high watermark. Pruning/checkpointing of that set and capture retention are unresolved integration work. This conservative adapter has unbounded retained history and increasingly expensive scans; its short-history results must not be extrapolated to months of operation. An optimized Turso adapter could use `sequence_watermark_experimental` after qualifying its exact semantics, rather than inventing a high-ID rule.

CDC blobs decode using the pinned engine's `ImmutableRecordRef`; their column positions still require schema/version metadata. SQLite JSON records include field names. Neither adapter converts arbitrary SQL into CRDT intent, validates capabilities, signs the canonical p2panda operation, or updates key state. The crypto scenario uses the same JSON serialization, Ed25519 signing + verification and ChaCha20-Poly1305 encryption + decryption on both engines; it persists the ciphertext text. It persists signature, nonce and ciphertext in the final source. It is an isolated common-cost control, not a full p2panda protocol or ratchet.

## Workloads and limits

Each partition has 1000 invoices, lines and tags, 64 author cursors and inventory items, identical indexes and INTEGER minor-unit amounts. Drafts query an indexed customer/status predicate and precisely update fields, tags and lines. Independent writers own separate 100-entity ranges and author rows. Contended writers share four entities and author row zero. Replication batches updates and marks them remote. Authority transactions perform stock reads, inventory decrements, unique dedup inserts, posting updates and effect-job insertion serially. Mixed mode runs three polling query observers per file; the final adapter polls author-log revisions and requeries on change. Catch-up runs a separate consumer per file while writers continue, then measures residual drain. Four-file tests place one writer per file and make no cross-file atomicity claim.

Transactions use BEGIN on SQLite/Turso WAL and BEGIN CONCURRENT on Turso MVCC. Busy/locked/stale-snapshot/conflict responses retry the entire mutation with bounded backoff and the same logical ID. Every successful case checks author-log totals against all requested mutations. No failed run enters throughput summaries. Repeated sequential file setup and seed insertion are outside writer timing; thread start and connection setup are inside the initial matrix wall interval. Warm-ups are ten transactions per file. FULL sync and fullfsync are requested on every connection. Final settings are recorded, and Turso MVCC source passes pager sync type to logical-log sync.

Latency covers begin through retries and commit, excluding final drain. p99 from 40 transactions per writer is noisy; longer follow-ups use 100. CPU and peak RSS cover the entire process (seed, warm-up, measurement, drain). The final retained-handle profile holds an active SQLite WAL connection and the Turso database open while sampling, including WAL/log bytes. Earlier matrix/focused/consumer storage samples may checkpoint on SQLite last-connection close and are unsuitable for cross-engine WAL size comparisons. Final profile additionally reports initial file sizes and logical image bytes. Historical capture lag was sampled indirectly by foreground consumed transactions and residual backlog/drain duration. The lag follow-up measures acknowledged commit to durable cursor consumption per transaction, including final backlog drainage, and reports p50/p95/p99 plus sample coverage. A consumer can finish before the writer records its acknowledgment timestamp; those races are counted as missing timing samples (up to six of 400 in the recorded cases), while transaction coverage is still verified. No capture-lag timing is inferred from the engine change_time field. Reader p95 includes polling/requery or consumer drain loops as labeled. The consumer reserves a WAL writer before scanning and advancing its cursor; MVCC uses BEGIN CONCURRENT. The historical matrix/focused consumer used deferred WAL transactions and frequently lost upgrades to foreground producers. WAL checkpoint and MVCC GC policies use engine defaults; there is no sustained retention plateau or checkpoint tuning experiment.

All main results are warm-cache **durable disk** runs. No cold-cache results are claimed. Memory and weaker NORMAL sync runs are separate profiles. Host activity, SSD controller behavior, thermal state, default checkpointing, short run length and different capture formats contribute variance. There is no physical power-cut test or P2P/network scalability extrapolation.

## Correctness coverage

`check` validates rollback leaves no published row images, savepoint rollback followed by commit leaves no images, expected complete transaction row count, restart before consumption, atomic durable cursor advancement, replay deduplication and remote-origin exclusion. MVCC holds an early transaction open while a later transaction commits and is consumed, then verifies the early transaction is consumed exactly once. The final check also counts rows an unsafe max-ID cursor would skip. `crash.py` waits for acknowledged commit, SIGKILLs the producer, reopens the file in another process and checks mutation totals plus restartable consumption.

Scope limits: no disk fault injection, physical power failure, arbitrary DDL/savepoint nesting, unsafe unmanaged connection writes, multi-consumer ownership protocol, schema migration across retained capture images, consumer crash mid-delivery to an external system, or p2panda shared-transaction trait integration. Local durable dedup is tested; exactly-once external delivery still requires an idempotent recipient. These are explicit qualification tasks, not hidden guarantees.
