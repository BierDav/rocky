# 3. Incremental finalization

Status: specified, not started. Owner: `rocky-secure` compaction and
constraints. Depends on [phase 1](01-held-positions-as-tables.md). Effort:
two to three weeks. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [lazy compaction](../runtime/lazy-compaction.md#finalizing),
[mergeable constraints](../runtime/mergeable-constraints.md),
[kept seals](../runtime/kept-seals.md) and
[review finding 2](00-sync-performance-review.md#findings).

After this phase finalizing a seal costs the operations the seal excludes
and the rows they touched. A seal in a quiet stream, which excludes nothing,
costs the index lookups that find its drops. The outcome is byte-identical
to today's double rebuild, and the full rebuild stays as the audit's and the
test suite's oracle.

## Settled

- **Rows know their operations.** `secure_row_positions(scope, row,
  position)` records, at apply time, every applied position that wrote a
  row; compaction below a base and eviction remove entries with their
  operations. Kept-seal rewinds and replay fill it like apply does.
- **An unchanged scope is its own sealed state.** When the seal excludes no
  applied operation of the scope, no row is restored or reprojected. Drops
  are found by index: tombstones through a new `live` column on
  `secure_rows` with a partial index, hidden constraint losers through the
  existing candidate tables, and `SET NULL` substitutions from the
  constraint graph of the dropped parents only. The kept path writes those
  drops into stored rows directly.
- **A changed scope rebuilds the rows it changed.** With excluded set `E`,
  the affected rows `R` are the rows `E` names in `secure_row_positions`,
  plus the rows whose constraint resolution depends on them (the `resolve`
  seeds of `R`). The transaction restores `R` at the frontier by replaying
  each row's included positions over its base record, reprojects `R`,
  computes drops over `R` and its dependents, records the diff, then
  replays `E` over `R` in application order. Every other row is untouched.
  Renumbering of excluded operations is unchanged.
- **One transaction, same order.** The finalization stays atomic with its
  bounds, drops and finalized record; failure leaves the previous state.
- **The oracle stays.** `audit` and a `cfg(test)` switch run the full
  rebuild and compare scope digests and typed tables with the incremental
  result. Every existing late-frame, kept-seal, existence, constraint and
  plateau probe runs with the comparison on.
- **Format.** `secure_row_positions` and the `live` column are mandatory;
  the replica file moves one version (combined with phase 1's bump when
  both land together, else `v34`).

## Ordered slices

1. `secure_row_positions` and `secure_rows.live`, written by apply, replay,
   rewind, import and eviction; the format bump.
2. Drops by index; the unchanged-scope path without rebuilds; the oracle.
3. The affected-rows rebuild for scopes with excluded operations,
   including constraint dependents and the kept path.
4. Guides: lazy compaction's finalizing steps and limits; scaling limits'
   known costs.

## Completion gate

- A 100,000-row scope: a seal excluding nothing finalizes in ≤ 200 ms; a
  seal excluding ten operations over 300 rows in ≤ 1 s; both equal the
  full rebuild's digests and typed tables.
- A scope with constraints: an excluded operation on a parent finalizes
  with the same children and support rows as the full rebuild.
- The phase 15 plateau probes and the two-device late-frame probe pass
  with the oracle comparison enabled.
- A kept seal reads the same rows before and after an incremental
  finalization of the next seal.
