# 6. Horizon, checkpoints and bootstrap

Status: complete; completion gate passed on 2026-10-09.
Owner: `rocky-secure` horizon, checkpoints and rebuild. Depends on phases
[4](04-partition-aware-constraints.md) and [5](05-partitioned-foreign-keys.md).
Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read requirement [5](scope-partitioned-constraints.md#5-horizon-checkpoints-and-bootstrap)
of the scope and [design rationale](../runtime/mergeable-horizon.md).
The starting implementation resolved dependencies per installed table scope. A child's statement embeds each parent's cut, and an
acknowledger must hold the parent at that cut; for in-group references that
is the parent's same-partition scope. The starting peer check covered only the finalized scope; phase 6 also
checks authorization for related parent partitions.

## Ordered slices

1. Horizon dependencies per group partition: a cut of `items` in list A
   depends on `lists` and `categories` in A only.
2. Finalization: hidden rows and orphans of a partition become final at the
   agreed horizon; cascaded children of a deleted parent are dropped with it.
3. Checkpoints: candidates stay derived and are not replicated. A checkpoint
   of one table and partition carries what its rebuild needs and nothing of
   another partition. Checkpoints already carry no candidates, support or
   hidden marks, so this is local rebuild work. Any change to the checkpoint
   is recorded in a decision record.
4. Bootstrap and rebootstrap of one group partition rebuild winners, hidden
   rows and support rows; other partitions are untouched.
5. A horizon in one partition never waits for a device that does not hold it.

Starting gaps in `rocky-secure/src/replica` (resolved below):

- `typed.rs` and `existence/base.rs` call `rebuild_constraints(None)` inside
  the finalize, rebootstrap and base-import transaction, rebuilding every
  table. Scope the rebuild to the affected partition.
- `horizon.rs`, `pending_horizons`, decodes every stored horizon and its
  checkpoint record on each write and frame admission. Per-partition horizons
  multiply that; persist a pending flag or index.
- Finalize is one transaction proportional to the scope's rows and filters
  `row_kind = 0` only on logged tables; unlogged support parents rely on
  virtual rows always having id 0.
- The checkpoint `format` number encodes which optional parts are present.
  After [phase 1](01-remove-compatibility-code.md) it is one version with
  optional fields; add per-partition state as a field, not a new number.

## Completion gate

- After the horizon, a deleted list's items and a lost duplicate title are
  gone from every device holding the list, and a long-offline device
  rebootstraps to the same rows.
- A partition is evicted and rebootstrapped from a checkpoint; its winners
  and hidden rows are rebuilt; another partition's rows, marks and hidden
  state are byte-equal before and after.
- A device holding only B finalizes B while A's horizon is still waiting.
- An interruption at each step of finalization recovers without a partially
  finalized partition.

## Implementation evidence

The inherited phases 4–5 already mapped dependencies to concrete in-group parent
scopes and scoped row reproject/eviction to a group partition. Phase 6 preserves
that behavior, verifies related-scope participant rights, scopes existence-base
imports to their connected constraint graph, and limits support-row collection
to rebuilt partitions. Full installation is the sole all-scope rebuild.

Pending declarations have a durable dependency index, updated atomically on
admission, checkpoint adoption and finalization. Per-write and frame admission
read only declarations affecting that scope. Finalization filters real rows on
both logged and unlogged support parents. [Decision 0043](../decisions/0043-partition-horizon-state.md)
records private replica identity v7; checkpoint 6 already carries independent
partition metadata and stays unchanged.

Three lifecycle regressions in `tests::partition_foreign_keys::horizon` cover:

- Two holders finalize unique losers, substituted category references and
  deleted-list cascade orphans; a device retaining the old rows imports the
  latest finalized group checkpoints and converges.
- Custody-backed group-partition eviction and ordinary checkpoint rebootstrap
  reconstruct visible winners and hidden candidates. Other partitions' stored
  rows, identity marks, candidates, typed rows, support and checkpoint state
  remain byte-equal.
- A keyless logged-parent consumer reconstructs support puppets from a partition
  checkpoint and imports an existence base without changing an unrelated graph.
- B-only holders finalize B while A waits; A-only peers and peers missing related
  parent rights are refused for B. A's related scopes stay sealed while B writes.
- Six deterministic finalization interruption boundaries roll back rows, stubs,
  derived state, checkpoint state and the pending index; reopening preserves the
  pre-finalization state before retry.

Finalization remains one transaction proportional to that scope's rows. Native
fixtures qualify this phase; Kotlin/Room partitioned schema admission remains
phase 7, and device/UI shopping qualification remains phase 10.

### Full gates on 2026-10-09

- `cargo fmt --all -- --check`, workspace Clippy with `-D warnings`, and
  `./scripts/commitlint --jj`: passed on the final source.
- `./scripts/check`: 655 workspace tests (including doctests) passed, 11 ignored;
  the SDK without iroh passed 128 tests, with 4 ignored. Zero failures.
- `cargo test -p rocky-secure --locked partition_foreign_keys`: all 11 tests
  passed, including the three new lifecycle regressions.
- `./sdks/native/check`: 2 tests passed; formatting, Clippy and the build
  without default features passed.
- `./kotlin check`: 51 JUnit tests passed, zero skipped or failed; all
  qualification and packaging tasks passed.
- Separate Room gate: 12 tests passed. Separate schema qualification: all
  7 hand/import pairs installed and reopened; the separate schema test gate
  passed 5 JUnit tests. Separate packaged consumer: one clean consumer run passed.
- The first Kotlin run identified stale Maven-local Android artifacts. The runtime
  was republished from this workspace before the successful rerun. Both configured
  Android ABIs built; no physical-device or UI qualification is claimed.

The native Phase 6 completion gate is satisfied. Finalization remains one atomic
transaction per table scope, proportional to its rows. Kotlin/Room partitioned
constraint admission remains phase 7; the complete on-device shopping and move
qualification remains phase 10. These later phases do not block this gate.
