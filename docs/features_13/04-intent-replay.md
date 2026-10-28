# 4. Intent replay

Status: planned. Owner: `rocky-core` (intent wire types,
`TableClass::Replayed`), `rocky-storage` (statement check and partition
split in `partition_sql.rs`), `rocky-secure` (authoring, receive, replay,
undo, outcomes), `rocky-sdk` and `sdks/uniffi` (schema declaration and
outcomes). Depends on [phase 2](02-interactive-first.md) and
[phase 3](03-intent-shapes.md). Effort: three to four weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

A table declared `Replayed` replicates intents. Replicas replay them in
canonical order, drop failures deterministically and report outcomes.
Concurrent increments both apply. This phase builds the engine for
replayed tables. Constraints beyond what SQLite enforces directly on the
projection come in phase 5; deletion of the old path comes in phase 8.

## Inputs

- The README's settled semantics and engine sections.
- `query-merge-prototype/rust-experiment/src/sealed.rs` and
  `convergence.rs`: canonical order, seal-relative drops and the
  termination argument. `tests/sealed_finalization.rs`: schedule
  fixtures.
- `rocky-core` `capture.rs` (`CapturedRow`, `SqlWrites`,
  `TransactionToken`), `rocky-storage` `partition_sql.rs`, `rocky-secure`
  `replica/author.rs`, `bundle.rs`, `apply.rs`, `incoming.rs`.

## Settled

- **Declaration.** `TableClass::Replayed`, with `coordinated: bool` on
  the logical table (only `Replayed` tables may set it). Schema install
  refuses foreign keys between `Replayed` and `Mergeable` tables, and from
  a coordinated table to an uncoordinated one.
- **Authoring.** The SQL session records each statement and its bound
  parameters alongside the captured rows. At commit, `rocky-storage`
  checks each statement against the determinism rules, splits it per held
  partition, and binds cross-partition subquery results. A refused
  statement fails the commit with a named reason; nothing was published.
  The local execution already happened on the author's snapshot and is the
  local result. The intent frame carries statements, parameters, stamp,
  base and partition; it carries no row images.
- **Receive.** An arrival ordered after everything applied in its
  partition executes directly, in its own savepoint. One ordered before
  applied transactions rewinds via undo records to its insertion point,
  applies, then redoes the suffix, each transaction in its own savepoint.
  A failing transaction is dropped with its reason; the rest proceed. The
  rewind is a quiet-gate step (phase 2) and is atomic to readers.
- **Undo records.** Per applied transaction in the open tip: the
  before-images of the rows it changed, written in the same transaction
  as the apply. They are deleted when a seal covers the transaction. They
  never leave the device.
- **Bundles.** A transaction spanning partitions is one bundle. It is
  ready when every member this replica holds is present. If a held member
  fails, the replica drops the whole transaction in every partition it
  holds.
- **Outcome records cross partitions.** A replica can't replay a member
  in a partition it holds no key for. So after its local commit, a
  replica that replays a member publishes a signed outcome record for
  it, `Applied` or `Dropped` with its reason, into that member's own log.
  The record names only the transaction and the member index, both
  already visible through the bundle's membership. A replica holding
  other members applies them provisionally and drops them when a
  `Dropped` record arrives; it never waits for one. Every holder of a
  partition computes the same outcome. Two records that disagree are
  refused as divergence and reported with both engine identities; they
  are never resolved by picking one.
- **Seals need complete outcomes.** A seal covers a multi-partition
  transaction only once outcome records for all its members are present
  at the sealing device. Missing records keep the transaction in the
  open tip, so its drops stay provisional.
- **Outcomes.** `TransactionToken` outcome: `Applied`, or
  `Dropped { reason, final }` with reason `Constraint`, `Admission`,
  `Schema` or `Engine`. Observers receive drops after the commit that
  records them. The Rust SDK and Kotlin binding expose both.
- **Late transactions.** A transaction based on a sealed tip that the
  seal does not cover replays in the successor tip, ordered after the
  seal's members. If its statements no longer compile there, it is
  dropped with reason `Schema`.
- **Routine seals.** An owner device seals when the open tip reaches
  `ROUTINE_SEAL_TRANSACTIONS` unsealed replayed transactions, using the
  existing `seal_followed` path. Slice 6 measures rewind cost per
  transaction and sets the constant so that a rewind across a full open
  tip on the slowest supported phone stays within features_12's
  transition reporting (record it; don't gate on it).
- **Statement budgets.** Carried from features_12 phase 4: single-row
  in-order intent receive ≤ 45 statements, single-row intent author
  ≤ 43. Per-group work runs once per group. Caches follow the phase 9
  invalidation rule. Admission is never skipped, moved after commit, or
  trusted across an authority, key or grant change.
- **Format.** Row format 12 → 13 and one replica format bump for intent
  frames, undo records and outcome records.

## Ordered slices

1. Intent wire types in `rocky-core`, `TableClass::Replayed`, the
   coordinated flag and schema install refusals. Format bump.
2. Statement check and partition split in `rocky-storage`, with tests for
   each refused construct, single-partition pinning, multi-partition
   split, cross-partition subquery binding, incomplete partitions and the
   64-partition cap.
3. Authoring intent frames through the bundle path; local result equals
   the captured rows. Author statement-count test (≤ 43).
4. In-order receive and replay in savepoints with drops and outcomes.
   Receive statement-count test (≤ 45). Concurrent `x = x + 1` from two
   authors yields both increments.
5. Undo records, rewind and redo for early-ordered arrivals; deletion of
   undo records at seal; late transactions in the successor tip.
6. Bundle readiness and outcome records across partitions; routine
   seals; rewind cost measurement and the seal constant.
7. Convergence tests: exhaustive schedules (every permutation and packet
   partition) for 4–5 transactions over two authors and two partitions,
   including drops, retransmissions and restarts mid-schedule; seeded
   schedules for 32 transactions over four authors. Compare state and
   drop map against a from-scratch canonical replay.
8. Crash tests at the qualification kill points during apply, rewind,
   redo and undo deletion: each reopens and converges.
9. Rerun L0 and L2 on desktop with a replayed shopping-app table, plus
   the desktop sync non-regression rows.

## Completion gate

- Convergence and crash tests from slices 7–8 pass within their tier
  budgets.
- Statement budgets asserted. 32-row receive stays ≤ 6.5 statements per
  row.
- Desktop L0 interactive write p95 ≤ 3 ms with a replayed table.
- Rewind cost per transaction and the chosen seal constant are recorded.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

Constraint semantics beyond plain SQLite execution and per-field stamp
removal ([phase 5](05-replayed-constraints.md)), the symbolic fast path
([phase 6](06-symbolic-fast-path.md)), migrations
([phase 7](07-intent-migrations.md)) and removing `Mergeable`
([phase 8](08-intent-only.md)).
