# Capture engineering assessment

| Responsibility | SQLite transactional triggers | Turso built-in CDC | Shared framework responsibility |
|---|---|---|---|
| INSERT/UPDATE/DELETE row coverage | Generate and maintain 21 triggers for seven tables | One per-connection enable statement; engine emits typed binary row images | Declare supported tables and tests |
| Before/after fields | Generated named JSON fields, schema-specific | Decode SQLite record blobs via pinned core API; position/schema mapping still needed | Retain schema/operation versions |
| Transaction boundary | Managed context + txmeta marker in source transaction | CDC v2 change_txn_id and commit record | Atomic metadata and logical author identity |
| Rollback/savepoint | Engine rolls back trigger staging | Engine rolls back captured rows; tested with both modes | Never deliver uncommitted output |
| Origin | Transactional context; remote rows omitted | Captured txmeta origin; remote images retained then filtered | Own every write connection/boundary |
| Safe durable consumption | Durable consumed-ID set + cursor | Same safe set here; engine experimental sequence watermark is an optimization candidate | Restart/replay, delivery acknowledgment, dedup |
| Schema migration | Regenerate triggers for changed columns; avoid stale JSON layouts | No trigger regeneration; captured schema changes help, but binary row version mapping remains | Test migrations and old operation formats |
| Retention | Custom staging/outbox pruning and consumed-set pruning | Custom CDC retention and consumed-set pruning | Checkpoint/bootstrap and consumer custody |
| Logical mutation semantics | Not derivable from row deltas | Not derivable from row deltas | Intent, CRDT semantics, policy, signatures, encryption |

The final source is roughly 700 lines of Rust including both adapters, schema, workloads, timing, crypto and correctness checks. This is **not** a production adapter cost estimate. SQLite generates 21 persistent trigger definitions from a compact common generator (about twenty source lines before formatting); Turso removes that generator and replaces it with connection setup, record decoding and CDC transaction joins. Durable origin/cursor/delivery logic remains in both. The number of saved trigger definitions is meaningful, but the short prototype does not demonstrate a major reduction in the total reliable framework integration effort. Avoid claiming a measured person-week saving from this harness.

Built-in CDC is a real maintenance advantage for a managed SQL bridge: row coverage is centralized in the engine, full before/after values are automatic, transaction grouping is explicit, and migrations do not require a fleet of hand-maintained triggers. Tests passed the important source-transaction and restart properties for v0.8.1. SQLite's generated capture works too; trigger generation moves table knowledge and edge-case qualification into Rocky. JSON staging additionally allocates and stores field names, whereas Turso's binary records are compact but couple decoding to the pinned engine/core format.

Turso CDC remains connection-local and must be enabled on every producer. Switching journal mode while active is rejected. The framework still needs a captured per-transaction metadata marker for origin and logical identity. Transactions that bypass the managed boundary are not qualified by this adapter. Consumers disable CDC to avoid observing their own progress writes. Full CDC also captures txmeta/outbox payloads; the complete-operation follow-up measures this duplication explicitly. The SQLite trigger allowlist omits that framework table. An origin marker separate from a large operation body could reduce Turso duplication, but this benchmark does not silently substitute that optimization. Remote materialization incurs retained CDC costs here; a dedicated remote connection with capture disabled could improve it, but then a separate explicit invalidation/revision path must cover observers. That optimization was not silently substituted in the replication benchmark.

For MVCC CDC, pinned `emit_cdc_change_id` allocates from an implicit AUTOINCREMENT sequence, registers in-flight allocations for a safe watermark, and synchronizes the compatible `sqlite_sequence` row in an autonomous inner transaction. The source explicitly documents serialization through that row. WAL CDC uses cheaper NewRowid allocation. The measured CDC-vs-disabled/explicit scaling captures the combined sequence, image, commit-record and durable-log overhead. It does not isolate time spent on each inner sequence conflict or fsync. Public retry counts omit engine-internal retries. A sequence-only microbenchmark or engine tracing would be necessary to attribute the whole difference specifically to shared-row contention.

Both engines need the p2panda store and projection/outbox changes to share one transaction; the existing SQLite implementation does not prove that boundary, and this harness does not implement p2panda traits. Turso needs a new store adapter and SQL/SDK/platform qualification. SQLite needs managed trigger migrations and its same-transaction store integration checked. No maturity preference is used to decide throughput or capture correctness.

Pinned implementation references:

- [CDC v2 configuration](https://github.com/tursodatabase/turso/blob/v0.8.1/core/cdc.rs)
- [CDC allocation and commit emission](https://github.com/tursodatabase/turso/blob/v0.8.1/core/translate/emitter/mod.rs)
- [Sequence allocation and compatibility row synchronization](https://github.com/tursodatabase/turso/blob/v0.8.1/core/translate/sequence.rs)
- [Safe sequence watermark used by upstream sync](https://github.com/tursodatabase/turso/blob/v0.8.1/sync/engine/src/database_sync_operations.rs)
- [MVCC durable-log commit implementation](https://github.com/tursodatabase/turso/blob/v0.8.1/core/mvcc/database/mod.rs)
- [SQLite WAL and sync semantics](https://sqlite.org/wal.html), [SQLite synchronous/fullfsync PRAGMAs](https://sqlite.org/pragma.html)

Selected source evidence is retained in `evidence/`. Upstream's safe watermark approach merits further implementation, but this report does not treat an experimental internal interface as a stable production cursor contract.
