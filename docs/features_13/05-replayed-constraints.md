# 5. Replayed constraints

Status: complete; see the [evidence](05-replayed-constraints-evidence.md). Owner: `rocky-core` (`logical.rs` installability and
the assertion declaration), `rocky-storage` (assertion checks),
`rocky-secure` (replayed row record, replay), `rocky-sdk` and
`sdks/uniffi` (schema declaration). Depends on
[phase 4](04-intent-replay.md). Effort: two to three weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Replayed tables get real SQL constraints: unique indexes, foreign keys,
complex `CHECK` expressions and cross-row assertions. SQLite or Rocky
checks them during each replayed transaction. A violation drops the
transaction; nothing is hidden, nulled or revealed later. Replayed projections keep no row records or per-field stamps; only
transaction undo is retained until sealing.

Today `installable` refuses every `CHECK` (`Incompatibility::CheckConstraint`)
and unique indexes need `SetNull` or `LatestWins`. Both exist because
registers merge fields separately and can build a row no transaction
wrote. Replay executes whole transactions, so neither reason applies to
replayed tables.

## Settled

- **CHECK constraints.** `Replayed` tables accept any column or table
  `CHECK` that SQLite accepts and that uses only the deterministic
  function allowlist. That includes multi-column arithmetic, `CASE`,
  `BETWEEN`/`IN`, `LIKE`/`GLOB`, string functions, `COALESCE`/`NULLIF`,
  JSON functions from the allowlist and generated columns referenced by a
  check. Schema install refuses a check that names a function outside the
  allowlist, with that function's name. SQLite semantics apply unchanged
  and are documented in the SDK. A generated column is declared as a
  `derived` column of the replayed table, stored by SQLite. Unique indexes
  may also declare expression terms and a `where` predicate in the derived
  rule grammar.
  - A check is evaluated per statement, not at commit. A transaction that
    passes through an invalid intermediate row fails even if its final
    state is valid; use one multi-column `UPDATE` instead.
  - A check whose expression is NULL passes. Use `NOT NULL` or
    `IS NOT NULL` when absence must be invalid.
- **Assertions for cross-row and cross-table invariants.** SQLite
  `CHECK` cannot contain subqueries, and per-table triggers are easy to
  get incomplete (the prototype's aggregate trigger covered only inserts).
  Rocky adds declared **assertions** instead: a name and a `SELECT` that
  returns violating rows, for example "a team has at most 10 members" or
  "an order's line total equals its header total". Rules:
  - An assertion reads only `Replayed` tables of one partition group and
    uses the deterministic allowlist. Schema install refuses anything else.
    The partition filter is appended by the same rewriter as reads, so it
    sees exactly the transaction's partition.
  - At the end of each transaction, before its savepoint is released,
    Rocky runs every assertion that reads a table the transaction wrote.
    Any returned row fails the transaction. It runs at author commit too,
    so a local violation is an ordinary commit error and nothing is
    published. During replay it is a drop with reason `Constraint` and
    the assertion's name.
  - An invariant across partitions cannot be checked by a replica that
    doesn't hold them all. Put that data in one partition, or in a
    coordinated table, which replays in log order and checks its
    assertions there.
  - Rocky does not support application triggers. Assertions are the one
    mechanism for invariants beyond a single row.
- **Unique indexes and foreign keys.** Plain SQLite enforcement on the
  replayed projection. `on_conflict` (`SetNull`, `LatestWins`) is refused
  on `Replayed` tables: the later transaction in canonical order is
  dropped instead. Foreign keys keep their declared `ON DELETE` actions;
  a cascade is part of the transaction that caused it and rolls back
  with it. Deferred foreign keys are checked at the transaction's end, as
  SQLite does at `COMMIT`. Rocky implements this by checking the deferred
  violation count before releasing the savepoint.
- **No hidden rows.** Replayed tables use none of `constraints.rs`'s
  candidate tables, support rows, virtual rows or `rocky_live` filtering.
  A child whose parent is missing fails its foreign key and is dropped.
  If the parent arrives later but is ordered earlier, the rewind redoes
  the child, which then succeeds. Replayed tables have no induced
  fan-out; the features_12 phase 2 exception never applies to them.
- **No field stamps.** Replayed tables keep no row record at all, so they
  have no per-field `stamps` and no stamp `authors` map.
- **Outcome records.** An outcome depends on partitions linked by foreign
  keys or assertions, so a record carries a digest of their prefixes and
  rewinds follow them. Row format 14.
- **Cost.** Statement budgets from phase 4 apply to tables without
  assertions. Each assertion run adds its own statements; the evidence
  records them per assertion. Assertions over large partitions need
  indexes the application declares; Rocky does not create them.

## Ordered slices

1. `installable` accepts allowlisted `CHECK`s and refuses `on_conflict`
   on `Replayed` tables; `CheckConstraint` stays refused for
   `Mergeable`. Tests for every refused function and for accepted
   complex expressions.
2. Replay with unique, foreign key (immediate and deferred) and check
   failures as drops, including cascades rolling back with their
   transaction and the intermediate-row and NULL cases above.
3. The assertion declaration in the logical schema, the SDK builder and
   the Kotlin schema DSL; install-time refusals (other partition group,
   non-replayed table, disallowed function).
4. Assertion evaluation at author commit and during replay, run only for
   assertions reading written tables. Tests: aggregate cap, header/line
   totals, an insert, update and delete each able to violate, and a later
   transaction that releases capacity letting an earlier-dropped one
   succeed after a rewind places it differently.
5. No field stamps on replayed rows, which keep no row record.
6. Convergence fixtures carried from the prototype's constraint tests:
   a composite many-to-many graph (720 permutations), complex checks
   (120), expression and partial unique indexes, cascades, and assertion
   caps, each against a from-scratch canonical replay.
7. Record statements per assertion and the replayed row record size
   against the register record.

## Completion gate

- All fixtures from slices 2, 4 and 6 converge to identical rows and
  drop maps under every tested schedule.
- No replayed-table code path calls the constraint resolver; a test
  asserts the induced-work counter stays zero for replayed tables.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

Cross-partition assertions, application triggers, automatic index
creation, and constraint changes for `Mergeable` tables, which phase 8
deletes.
