# Rocky DB engine decision

Status: superseded by [design rationale](../../docs/runtime/write-concurrency.md#engine) on 2026-10-04; kept as historical evidence.

**Recommend Turso for concurrent independent mutations in one database file, with MVCC and a framework-owned explicit logical-operation/outbox path.** Its measured advantage is material enough to justify a new storage adapter and targeted compatibility qualification. **Choose SQLite WAL if Rocky retains one serial writer per file or partitions independent work across files.** This is a conditional foundation recommendation; the workload mix determines the choice.

Do not enable full CDC for every MVCC projection write by default. Turso WAL CDC is a useful managed SQL bridge. Full CDC on MVCC is correct in the tested cases but costly enough to remove its concurrent-write advantage.

## Evidence

The predeclared threshold was at least 25% throughput improvement without materially worse transaction p99, or substantially simpler reliable capture integration. Three repeated release runs meet the performance threshold for explicit concurrent mutations:

| Workload | SQLite explicit | Turso MVCC explicit | Interpretation |
|---|---:|---:|---|
| Four independent writers, one file, complete JSON operation bodies | 222 tx/s | 645 tx/s | 2.91×; p99 330 → 9 ms |
| Eight writers, batch four, one file, compact markers | 291 tx/s | 1,215 tx/s | 4.17×; confirm with realistic payloads before sizing |
| Four writers across four files, batch four | 358 tx/s | 329 tx/s | No Turso advantage once each file has one writer |
| Serial common crypto, batch four, complete bodies | 248 tx/s | 223 tx/s | No Turso advantage |

These are median run values on an Apple M5 Pro, macOS/APFS, warm small datasets and FULL/fullfsync disk durability. Logical mutations/s is transactions/s times batch size. Mixed revision observers preserved the MVCC write advantage but observer p95 regressed from about 0.17 ms on SQLite to 8.2 ms on MVCC. CPU and memory also increased. Strict reader/resource budgets can reverse the choice.

## Built-in CDC counts as an advantage

Turso eliminates 21 generated triggers in this schema and centralizes typed before/after images, transaction IDs and commit records. This is a meaningful correctness and maintenance benefit for a managed SQL bridge. All six adapters passed tested rollback, savepoint, committed grouping, restartable cursor, replay/dedup, remote-origin exclusion and acknowledged-write process-crash recovery. Turso 0.8.1 accepts MVCC followed by CDC and rejects journal switching while CDC is active.

Reliable integration still needs origin metadata, schema/version mapping, a durable consumer, retention and the framework's logical intent, signatures and encryption. The prototype removes a compact trigger generator; it does not establish a major saving in total framework implementation effort. CDC also captures framework metadata payloads, adding duplication cost.

Complete-operation independent capture runs measured about 168 tx/s for SQLite triggers, 271 for Turso WAL CDC and 75 for MVCC CDC. WAL CDC had worse transaction p99 than triggers. MVCC CDC sequence code documents a shared compatibility sequence row and autonomous allocation transactions; capture-disabled controls show substantial combined capture overhead. Internal sequence retries were not separately instrumented, so not all overhead is attributed to sequence contention.

## Qualification and boundaries

A delayed-commit test demonstrated that a max-change-ID cursor would skip two rows. The adapter's durable consumed-transaction set avoids that error, but retained-history scans and pruning are not yet a bounded production cursor design. Upstream's experimental sequence watermark deserves qualification. Catch-up measurements include actual consumption and durable advancement; a WAL reservation follow-up fixed read-upgrade starvation. Direct lag, CPU/RSS, storage growth, failures and sample ranges are preserved.

Before adopting Turso, qualify Rocky's actual SQL/migrations/platform APIs and implement the p2panda store over the same transaction as projections and the signed outbox. The existing SQLite store also needs that atomic-boundary check. These costs are justified when independent one-file concurrent writes are a real bottleneck; the serial/partitioned topology has no demonstrated payoff from changing engines.

There is insufficient evidence for mobile battery/platform behavior, physical power loss, cold-cache operation or months of retained capture history. This is local-engine evidence, not overall P2P scalability.

[Reproduce the benchmarks](README.md), [capture engineering assessment](INTEGRATION.md), [detailed evidence and limitations](ASSESSMENT.md), [raw profile descriptions](results/PROFILES.md), [summary and ranges](results/summary.csv), [comparison charts](charts/throughput.png), [complete-operation comparison](charts/logical-payload.png).
