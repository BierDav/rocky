# 4. Intent replay: evidence

## Slice 2: statement check and partition split

Status: done (2026-10-29), in `rocky-storage` only.

`partition_sql/intent.rs` adds `split_intent(sql, params, views, tables,
evaluate)`. It checks one application write to a replayed table and returns
one `PartitionStatement { partition, statement: Checked }` per held partition
it reaches, in `held` order. The existing rewriter appends each copy's
partition filter and binds its values.

- Determinism: INSERT, UPDATE, DELETE and upserts only. The allowlist
  `DETERMINISTIC_FUNCTIONS` covers scalar, exact-math, aggregate, window and
  JSON functions; transcendental math is excluded because it depends on the
  platform's libm. Date and time functions are allowed only with bound
  arguments that are not `now`, `localtime` or `utc`. Refusals are typed
  (`IntentRefusal`): `Clock`, `Random`, `Environment`, `Extension`,
  `UnknownFunction`, `UnknownCollation`, `RowId`, `Schema`, `Pragma`,
  `Attachment`, `NotAWrite`, `PartitionMove`, `CorrelatedSubquery`,
  `Unavailable`, `TooManyPartitions` and `Unsupported`.
  `check_replayed_table` refuses `AUTOINCREMENT`.
- Split: an UPDATE or DELETE reaches the held partitions its filter pins, or
  every held one. INSERT rows are grouped by their bound partition values.
  A partition that is held but waiting refuses the write (`Incomplete`). A
  pinned partition that is not held refuses it (`NotHeld`). More than
  `MAX_INTENT_PARTITIONS` (64, equal to `MAX_BUNDLE_FRAMES`) refuses the
  statement. An unpartitioned table is one partition.
- Reads: a subquery or INSERT source that reads only the copy's own partition
  stays SQL, so replay re-reads it. It qualifies when it is pinned to the
  partition or equates the partition columns with the outer row's. Any other
  read runs once through `evaluate`, with the held filters applied, and is
  bound: a scalar becomes one value, `IN` a value list, `EXISTS` 0 or 1, and
  an INSERT source its rows. A read of other partitions that names the outer
  row is refused as `CorrelatedSubquery`.

Tests (`partition_sql::intent::tests`, unit tier, in-memory SQLite):
`clock_functions_refuse_unless_fully_bound_without_now`,
`random_functions_refuse`,
`environment_functions_and_engine_tables_refuse`,
`unknown_functions_and_collations_refuse`, `extension_loading_refuses`,
`rowid_references_refuse`, `autoincrement_schemas_refuse`,
`schema_statements_refuse`, `pragmas_and_attachments_refuse`,
`reads_are_not_writes`, `partition_column_writes_refuse_as_moves`,
`a_filter_that_pins_one_partition_yields_one_copy`,
`a_statement_reaching_several_partitions_splits_into_equivalent_copies`,
`reads_of_other_partitions_are_evaluated_once_and_bound`,
`held_partitions_that_wait_refuse_the_write`,
`more_than_sixty_four_partitions_refuse_the_statement`. The equivalence tests
run the original on the held partitions and every copy on a device that also
caches an unheld partition. The resulting rows must be equal, and the unheld
row must be untouched.

Left for slice 3:

- `evaluate` must read the snapshot the statement executed on. Run the split
  when the statement executes, not after later statements of the
  transaction.
- The 64-partition cap applies per statement. The bundle's union of
  partitions across statements is checked by the caller.
- An omitted `INTEGER PRIMARY KEY` gets a rowid on each replica. Refusing it
  needs the table schema.
- Refused as `Unsupported`: `WITH`, `UPDATE … FROM`, `LIMIT` on writes,
  named parameters, and partition values that are not literals or bound
  parameters.
- A read of another table that refers to an outer column without a
  qualifier fails in `evaluate`; it does not get a named refusal.

Checks, on base `utyqryskqtlr` (`bfcbb0269`), macOS, host load 112–167 on 18
cores from parallel agents:

- `./scripts/lint`: passed.
- `./scripts/check --package rocky-storage`: clippy passed, and the unit tier
  passed (134 tests, 21.5 ms load-scaled, including all slice-2 tests). The
  integration tier ran 1391 tests and exited 1. Under load, rocky-sdk
  `three_failed_advised_imports_fall_back_only_for_that_scope_and_checkpoint`
  hit the 30 s timeout; it passed alone in 1.42 s. Seven rocky-secure
  integration tests went over the 2 s budget (`a_writer_commits_within_the_write_limit_during_a_catch_up`
  was at 2.03 s alone). None of them reach the new module, and the only change
  to existing code is a refactor of `table()` that keeps its behavior.
