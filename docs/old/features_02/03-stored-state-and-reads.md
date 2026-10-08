# Phase 3 — Stored state and indexed reads

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-04 with stated limits; see the
[outcome](#outcome-2026-10-04). The brief text is unchanged.
Proposed after [phase 1](01-baseline-and-hardening.md).
Context: [supported API](../../runtime/supported-api.md),
[app runtime](../../runtime/app-runtime.md), [scaling limits](../../runtime/scaling-limits.md#reads-and-observers).
Note 2026-10-04: [phase 2](02-sqlite-engine.md#outcome-2026-10-04) missed two gates
that this phase must meet. A point read at 2,000 tables takes 82 ms against
0.79 ms at 20, and writes wait up to 0.7 s behind observers' reads, because reads
rebuild every authorized table per revision and run on the file's writer thread.

Today the trusted worker keeps the whole application state in memory, clones it
for every mutation, and copies the authorized subset into a new in-memory database
for every read. Write and read cost therefore grow with data size, and reads above
4,096 rows are refused. This phase makes stored rows the state.

## Design constraints

- The reducer stays deterministic and application-owned. It must be able to run
  against only the rows an operation touches.
- Authorization stays application-defined. A read must never see protocol tables,
  another namespace or rows the policy denies.
- `admit_candidate` currently receives whole before and after states. Its
  replacement receives the touched rows before and after.
- The stored projection must still be reconstructible from retained history plus,
  after phase 4, a checkpoint.

Write a decision record before coding. It must choose the state access contract
(for example a row-level `load`/`store` interface scoped to declared tables),
how read authorization is expressed without materializing (for example declared
row filters compiled into the query, or authorized views), and how existing
applications and files move across. Changing `StoredApplication` is expected;
keeping a compatibility adapter for small in-memory apps is acceptable if it is
clearly bounded.

## Slices

1. **Row-level state access.** Introduce the contract and port the inventory app.
   A mutation loads, validates and writes only touched rows inside its transaction.
   Remove the per-mutation state clone for ported apps.
2. **Port remaining apps.** Todo, alice, accounting, employees, directory and the
   ledger example. Keep behaviour and convergence tests unchanged.
3. **Direct authorized reads.** Run bound queries against stored rows in a read
   transaction with the authorization filter applied by the runtime, not by a copy.
   Keep the existing statement restrictions. Replace the row-count refusal with
   paging: `max_rows`, a continuation cursor and a byte limit.
4. **Declared indexes.** Applications declare indexes in their schema; the
   fingerprint covers them. Verify with query plans in tests that the probe's point
   and range reads use them.
5. **Incremental observers.** Track which tables a mutation changed. Wake only
   observers whose query reads a changed table, run each distinct query once per
   revision, and deliver the shared result. Slow observers still coalesce.
6. **Reads off the write thread.** Serve reads from separate read connections so a
   long query does not delay commits. A snapshot must correspond to one committed
   revision, and its reported revision must be that one.
7. **Encrypted path.** `EncryptedReplica` already stores materialized rows. Audit
   its reads against the same budgets, add paging and indexes where the phase 1
   measurements show a need, and remove whole-scope materialization from its
   read and rebuild paths.

## Gate

At 500,000 rows in one domain across the wide probe schema:

- median write cost is within 2× of the empty domain;
- an indexed point read is under 1 ms median and a paged range read is
  proportional to the page;
- with 120 observers, write throughput is within 20% of the no-observer figure
  when their queries do not read the changed table;
- resident memory does not grow with row count beyond the engine's page cache;
- a denied row, a protocol table and a foreign namespace are unreachable through
  every read shape, including paging cursors;
- convergence, rollback, observation-race and reopen tests pass for every app.

History is still fully retained, so loading 500,000 rows needs the phase 1
measurement quota override. That is expected here; phase 4 removes it.

## Out of scope

Pruning history, joins or new SQL shapes in user reads, and cross-domain queries.

## Outcome, 2026-10-04

All seven slices are implemented.
[Decision 0009](../../decisions/0009-stored-state-and-authorized-reads.md) records
the design; the guides are [app runtime](../../runtime/app-runtime.md#read-authorized-local-data)
for the trusted path and [permissions and sync](../../runtime/permissions-and-sync.md)
with [scaling and rebuilds](../../runtime/scaling-and-rebuilds.md#local-projection-rebuild)
for the encrypted one.

| Slice | Where |
| --- | --- |
| 1, 2 State access, apps | `StoredApplication::touched`/`window` in `rocky-core/src/storage.rs`; todo, inventory, directory, the ledger example and both probe apps. The worker keeps counters and a frontier |
| 3 Direct authorized reads, paging | `read_views`; `rocky-runtime/src/read.rs`; `ReadOptions::max_bytes`/`cursor`, `ReadSnapshot::next`, `ReadView::key`, `CursorKind`; SDK `cursor`/`max_bytes`/`next`/`next_kind` |
| 4 Declared indexes | Indexes in `schema()`; `Replica::explain_read`; plan tests in `read_tests.rs` and the probe |
| 5 Incremental observers | Per-view change counters and shared results in `read.rs` |
| 6 Reads off the write thread | `ReadPool` in `read.rs` |
| 7 Encrypted path | `snapshot_page`, bounded `snapshot`, point `companion`, shadow-table rebuild and an index under `secure/replica/` |

Alice, accounting and employees are `RowApplication`s on the encrypted path and
were already row-level for mergeable rows; they needed no port.

### Gate

Figures are one release run each: [trusted](../../runtime/scaling-limits.md#after-stored-state),
[encrypted](../../runtime/encrypted-baseline.md#after-stored-state).

| Gate | Result | Evidence |
| --- | --- | --- |
| Median write within 2× of the empty domain | Met | 143 against 146 µs at 2,000 tables; 129 against 117 µs at 20 |
| Indexed point read under 1 ms median | Met | 35 to 40 µs at 2,000 tables. Encrypted: 78 µs at 4,000 rows |
| Paged range read proportional to the page | Met for keyset cursors | 12,500 rows deep: 30 / 54 / 285 µs for 10 / 100 / 1,000 rows by keyset cursor, against 32 / 57 / 280 µs for first pages. Queries outside the [keyset shape](../../runtime/paged-reads.md#when-a-read-gets-a-keyset-cursor) page by offset: 579 / 589 / 668 µs. Encrypted pages use a key cursor: 184 to 197 µs for 10 rows at 500 to 4,000 rows |
| 120 observers of another table within 20% | Met | 6,326 against 6,208 writes/s; 4,391 when they read the edited row |
| Resident memory does not grow with rows | Met | 62 to 66 MiB at 500,000 rows against 68 empty, 2,000 tables |
| Denied row, protocol table, foreign namespace unreachable, cursors included | Met | `read_tests.rs`, `read.rs` tests; encrypted `secure::tests` and `tests/scaling.rs` |
| Convergence, rollback, observation-race and reopen tests pass | Met | `./scripts/check` passes |
| Phase 2: point read at 2,000 tables within 2× of 20 | Met | `wide` rerun: 20 against 24 µs median at 3,500 rows; one first read took 9.7 ms |
| Phase 2: no wait beyond 318 ms | Met | `wide` rerun at 2,000 tables: largest write wait 32 ms, with 8 observers 127 µs p99; before, 653 to 703 ms |

`scaling_probe -- all` and `./sdks/kotlin/check` were rerun after the change and pass.

Two existing tests changed on purpose: a result longer than `max_rows` is a page
with a cursor, not a quota error. `RuntimeOptions::read_cache` is removed.

### User decision: keyset cursors

The trusted read cursor was first a row offset, which missed the paging gate for
deep passes. On 2026-10-04 the user chose the hybrid recorded in
[decision 0009](../../decisions/0009-stored-state-and-authorized-reads.md#paging):
views declare a key, ordered plain queries continue by keyset cursor, and every
other accepted query keeps the offset cursor. The trade-off for consumers is in
[paged reads](../../runtime/paged-reads.md). `ReadOptions` and `ReadCursor` are no
longer `Copy`.

### Remaining limits

- `query()` and `observe()` still return the whole projection on the writer thread.
- Reads outside the keyset shape, such as grouped results or descending orders
  over columns not declared never NULL, page by offset.
- A strong command replays its sealed cut in memory; open replays history within
  the 4,096-operation quota and only reads counters above it. Open still scans
  `rocky_operations` once for its budget.
- A bound read no longer waits for queued, uncommitted writes of its domain.
- With 2,000 tables every write prepares its statement again: the adapter caches
  128 statements. Not profiled.
- Encrypted: the record quota stays 4,096, so that path cannot reach 500,000
  rows before [phase 4](04-checkpoints-and-pruning.md). Reads wait behind the
  replica mutex. The open-time audit replays a keyed scope in memory. Command
  state is one record per scope and command reads scan all retained results;
  changing that changes a wire format and was not done.
- The todo desktop demo was not started interactively.
