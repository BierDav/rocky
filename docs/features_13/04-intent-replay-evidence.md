# 4. Intent replay: evidence

## Slice 1: replay descriptors and intent wire types

Status: done (2026-10-29).

Landed `TableClass::Replayed` and the coordinated flag; install refusals
`Incompatibility::{Coordinated, ClassReference, CoordinatedReference,
Unsupported(Replayed)}`; and `rocky_core::replay` intent, base, outcome and
drop types. `ROW_FORMAT` changes from 12 to 13 and replica `FILE_IDENTITY`
from v39 to v40.

Coverage includes mixed/coordinated reference refusals, unsupported replayed
installation, invalid statement counts and base ordering, final-drop
encoding, and unsupported replica/frame format refusals before writes.

Checks: formatting, Clippy and lint passed. `./scripts/check --changed`
passed all 219 unit and 1,471 integration tests functionally and exited 1
only on tier time budgets, on a loaded 18-core host with the data volume
about 96% full; the same overruns appear on the base.

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

## Slice 3: authoring intent frames

Status: done (2026-10-29), authoring only; receivers refuse intent frames
until slice 4.

- Replayed tables are typed: `TableClass::typed()` covers `Mergeable` and
  `Replayed`, so they get scopes, keys, partitions, manifest entities and a
  place in the replicated fingerprint (with their foreign keys). The SQL
  capture writes and captures them like mergeable tables; the commit
  publishes no row images for them.
- Each write to a replayed table is split (`split_intent`) under the open
  transaction before it runs, so reads of other partitions see the
  statement's own snapshot. Copies map to partition scopes and must be
  writable by the session. `ROLLBACK TO` drops the copies recorded after
  its savepoint. A statement that changes a replayed table without copies
  (one the splitter cannot name) is refused and poisons the transaction.
- At commit the copies group per scope into one intent frame each, beside
  the mergeable row frames, in one bundle (`author_sql_bundle`). Moves,
  publications, re-authoring and coordinated writes with replayed changes
  are refused.
- The bundle body carries `intent: Option<Intent>` (base and statements)
  instead of writes; the frame header already carries the stamp, tip and
  partition, so `rocky_core::replay::Intent` dropped them. Decoding refuses
  an intent body with rows, carried proofs or no membership.
- Base and order: an intent frame depends on every author's last applied
  frame in the scope (at most 64), names them as its base, and its clock
  follows all of them, so canonical order puts it after what it read.
- Install: the secure profile accepts replayed tables only with
  `rocky-core/intent-replay`, which only `rocky-secure`'s tests enable,
  until slice 9 lifts the refusal.

Tests (`rocky-secure` `tests::sql::intents`):
`replayed_writes_commit_as_intents_beside_mergeable_rows` (mixed bundle,
savepoint rollback, local result, statement text and parameters, base and
clock of the next intent),
`nondeterministic_replayed_writes_are_refused_before_they_run`,
`a_single_row_intent_authors_within_its_statement_budget`,
`receivers_refuse_intent_frames_until_they_replay`.

Statement counts for one single-row autocommit `UPDATE` (`BEGIN` through
`COMMIT` / whole call including session admission around it):

| Table | Transaction | Whole call |
| --- | --- | --- |
| Mergeable `items` | 74 | 112 |
| Replayed `salaries` | 41 (≤ 43) | 79 |

Left for later slices:

- Receive and replay (slice 4). Until then a receiver rejects an intent
  frame by name and applies nothing.
- Admission on replayed effects, including the author's own rows (slice 4).
- Replayed tables keep no row records yet: rebuild, restore and moves of
  replayed rows need the replay engine (slices 4–5).
- The base reads `secure_operations`; frames pruned under a checkpoint
  drop out of it and rely on the seal that covers them (slice 5).
