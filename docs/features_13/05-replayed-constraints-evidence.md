# 5. Replayed constraints: evidence

Measured on change `vnwvyqpp` (parent `b15b530b`), macOS on an 18-core host,
debug test profile, 2026-10-30. Statement counts are deterministic; none are
timing measurements.

## Slices 1 to 4: declaration and enforcement

`installable` accepts allowlisted checks and refuses `on_conflict` on replayed
tables. Assertions are declared in the logical schema, `rocky-sdk`
(`Schema::assertion`, `TableBuilder::check`) and Kotlin (`assertion`, `check`,
`reference(..., deferred = true)`, `generated`, `Index.expressions`,
`Index.where`). References between replayed tables are
accepted by Kotlin as they already were by the core.

`tests::replayed_constraints` covers unique, check, immediate and deferred key
drops, cascades rolling back, the intermediate-row and NULL cases, and an
insert, update and delete each violating an assertion, including a drop
released by a rewind.

## Fix: outcomes depend on linked partitions

Rewinds covered one partition, so a late parent delete left a dependent
insert in another partition stale. `rewind` now follows partitions linked by
foreign keys or assertions (`linked`, `linked_scopes`).

The 720-order graph then showed the same gap in outcome records: their prefixes
named only the member's partition, so an insert replayed before its parent
arrived dropped with the same prefix its author applied under, and receivers
rejected honest records as divergent. `OutcomeRecord.linked` now carries a
digest of the linked partitions' prefixes, and records only compare when it
matches. It is a fixed-size digest because a link can reach every partition.
`ROW_FORMAT` is 14.

## Slice 5: no field stamps

Replayed tables keep no row record: `secure_rows` holds zero rows for them
(asserted in `statements_per_assertion_are_recorded`), so there are no
per-field stamps or stamp authors to remove and no replayed record format.

## Slice 6: convergence fixtures

`tests::replayed_constraints::convergence`, e2e tier. Each history is a setup
and units of concurrent commits, compared with a from-scratch canonical replay
in plain SQLite, on rows and drop map. Phase 6 runs over the same `History`
values.

| Fixture | Schedules | Dropped |
| --- | --- | --- |
| Composite many-to-many graph (partitioned, composite deferred keys) | 720 orders | 3 of 6 |
| Complex checks | 120 orders | 3 of 5 |
| Expression and partial unique indexes | 192 (orders and packet splits) | 1 of 4 |
| Cascades | 192 | 1 of 4 |
| Assertion caps | 192 | 1 of 4 |

A check that fails at its own author is a refused commit here, so these
histories fail through concurrency instead of the prototype's standalone bad
writes. Identity updates and the prototype's trigger are replaced by deletes
and the `line_cap` assertion. All five declare their constraints through the
public definition; the complex-checks fixture asserts a computed `net`.

## Slice 7: cost

Statements one receiver spends on one transaction
(`statements_per_assertion_are_recorded`):

| Transaction | Statements |
| --- | --- |
| One insert, no assertion, no deferred key | 99 |
| One insert, one assertion | 103 |
| Two members, a deferred key and an assertion | 379 |
| Receive that rewinds and redoes past one assertion | 108 |

One assertion adds 4 statements. Size of one three-column row: register record
170 bytes in `secure_rows`; replayed row 0 bytes there and at most 48 bytes of
undo until sealing.

## Generated columns and index terms

A `derived` column of a replayed table is SQLite's own stored generated column,
so a check can read it; `derived` keeps its meaning on other tables. Capture
binds through `table_xinfo`, and undo and schema rebuilds never write it.

`IndexDefinition` gains replayed-only `expressions` and `where`, carried by
`LogicalIndex`, and both SDKs declare them. They use the derived-rule grammar,
so kept history binds the same terms and records exactly the rows a unique
index clashes on; `Unique::clash` has a regression test. Checks, generated
rules and index terms refuse statement separators, subqueries and unbalanced
parentheses (`replayed_tables_declare_generated_columns_and_index_terms`).

## Fix: a read racing a commit

The shopping suite failed intermittently with `submission identity reused with
different input`. A commit advances `secure_read_epoch` before its lock holder
refreshes readers, so a cursor opened in between failed its readiness check.
`SecureSql::current_read` now waits for the lock and plans again, at most 16
times. `a_cursor_racing_a_commit_reads_once_readers_are_refreshed` fails
without the retry.

`SealsTest` and `SchemaUpgradeTest` also assumed one pass was enough: compaction
needs frames the titles do not show, and a reopened peer can miss one attempt.
Both now wait for the state they assert, bounded.

## Completion gate

All on the final change, with the SDK republished to Maven local first, because
the shopping app resolves Rocky from there rather than from the working copy.

| Check | Result |
| --- | --- |
| `./scripts/lint` | passed |
| `./scripts/check` | passed: 246 unit, 1,515 integration |
| `cargo test -p rocky-secure --lib replayed_constraints` | 10 passed, including all five fixtures |
| `sdks/kotlin/gradlew -p sdks/kotlin check` | passed |
| `./examples/shoppinglist-app/check` | passed: 25 Android and 70 JVM tests, all modules build |
| Shopping JVM suite repeated | 5 of 5 runs, 70 of 70 |

Fixtures converge to identical rows and drop maps under every schedule. No
replayed path resolves constraints: the foreign-key test asserts
`resolves_constraints` is false and the dependent-row work counter stays zero.

Not verified: iOS (`check-ios`), phones, the shopping multi-process
qualification, and the e2e tier other than these fixtures.
