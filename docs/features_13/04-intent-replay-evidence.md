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

## Slice 4: in-order receive and replay

Status: done for in-order arrivals (2026-10-29). Early-ordered arrivals wait
(`WaitReason::Rewind`) for slice 5.

- A received bundle's intent members replay together inside the bundle's
  savepoint, after an order check: each must follow the last transaction its
  partition replayed (`secure_replayed`, by `SecureStamp`). An earlier one
  waits, and reads of its partition refuse as incomplete meanwhile.
- The receiver splits every statement again, confined to the frame's own
  partition, and runs only those copies, with the session authorizer and
  capture armed. A statement that no longer splits drops the transaction
  as `Schema`; a constraint failure as `Constraint`; anything else the
  engine refuses as `Engine`.
- Admission runs on the replayed effects: every captured row's before and
  after image must be permitted by the frame author's grants, in the
  frame's own scope, else the transaction drops as `Admission`. The author
  checks its own effects the same way at commit, so a session cannot keep
  a local result replicas would drop.
- A drop rolls back every intent member of the bundle; the frames still
  apply as log entries, so the author's log continues. `secure_replayed`
  records each member's order and outcome; the author records its own
  transactions as applied at commit. `EncryptedReplica::transaction_outcome`
  returns `Applied` or `Dropped { reason, final: false }`; finality needs
  the seal coverage of slices 5–6.

Tests (`rocky-secure` `tests::sql::intents`):
`concurrent_increments_replay_in_order_and_both_apply` (two authors raise
the same amount; a third replica and the earlier author get both
increments; the later author's earlier arrival waits to rewind and its
partition reads refuse), `a_failing_replay_drops_its_transaction_and_the_log_continues`
(duplicate key drops as `Constraint`, a later raise replays on the winner,
outcomes per replica), `replayed_effects_outside_the_authors_grants_drop_as_admission`,
`a_single_row_intent_receives_within_its_statement_budget`.

Statement counts, one single-row transaction (whole call / its own
application or transaction):

| Path | Mergeable | Replayed |
| --- | --- | --- |
| Author (`BEGIN`…`COMMIT`) | 112 / 74 | 80 / 42 (≤ 43) |
| Receive (`received_application`) | 93 / 39 | 85 / 31 (≤ 45) |

Left for later slices:

- Rewind and redo for early-ordered arrivals, undo records, late
  transactions (slice 5).
- Bundles mixing replayed and mergeable members apply the mergeable rows
  even when the intents drop; whole-transaction drops across members and
  outcome records across partitions are slice 6.
- Outcome exposure through `rocky-sdk` `Transaction::commit`, observers and
  the Kotlin binding.
- Each replay binds the plan's physical columns again (two
  `pragma_table_info` reads); a cache would follow phase 9's rule.

## Slice 5: undo records, rewind and redo

Status: done (2026-10-29). Late transactions order after the sealed members
of their partition; replay in a successor tip after a tip change is not
exercised yet.

- Every applied member keeps the before-images of the rows it changed in
  its `secure_replayed` row (`undo`, written by the same statement that
  records its order and outcome, so authoring stays at 43 statements). A
  receiver takes them from the replay's capture; the author from its
  session's capture at commit. They never leave the device.
- An arrival ordered before unsealed applied transactions of its partition
  rewinds them inside the bundle's transaction: latest first, each
  member's changed rows are deleted, then its before-images inserted with
  foreign keys deferred. Members of a rewound transaction in other held
  partitions rewind with it, and so does what follows them there. The
  arrival then replays, and the rewound transactions redo in canonical
  order, each in its own savepoint with admission against its author's
  current grants (an author that no longer verifies drops as `Admission`).
  A redo can change an outcome: a later create that applied first drops
  once the earlier one arrives.
- The rewind runs inside the receive transaction under the replica lock,
  so readers never see a rewound partition. `WaitReason::Rewind` is gone.
- When a seal finalizes a scope through a position, the replayed
  transactions it covers are marked sealed and lose their undo images; they
  never rewind again and their drops report `sealed: true`. An arrival
  ordered before a sealed transaction replays after the seal's members.

Tests (`rocky-secure` `tests::sql::intents`):
`concurrent_increments_replay_in_order_and_both_apply` (the later author now
rewinds his raise, applies the earlier one and redoes his: every replica
holds both increments and three undo records),
`an_earlier_arrival_rewinds_and_redo_reevaluates_later_outcomes` (Bob's
create and raise apply first; Alice's earlier create rewinds them, Bob's
create redoes as a `Constraint` drop, his raise lands on Alice's row; a
replica receiving in canonical order agrees), and
`a_seal_ends_rewinds_and_makes_drops_final` (sealing through the applied
prefix drops undo images and finalizes drops; a later arrival applies on
top). The seal test drives `seal_replayed` directly; the compaction path
calls it where it records seal bounds.

Statement counts, one single-row transaction:

| Path | Mergeable | Replayed |
| --- | --- | --- |
| Author (`BEGIN`…`COMMIT`) | 112 / 74 | 81 / 43 (≤ 43) |
| Receive (`received_application`) | 93 / 39 | 85 / 31 (≤ 45) |

Left for later slices:

- Replay of a late transaction in a successor tip after a tip change, and
  its `Schema` drop when its statements no longer compile there.
- Rewind cost per transaction and the routine seal constant (slice 6).

## Slice 6: bundles across partitions, outcome records, routine seals

Status: done (2026-10-29). The roadmap's settled text on outcome records
was revised with this slice: records travel in the publisher's own held
partitions, not in the log of the member they describe.

- A transaction writes replayed tables or mergeable ones, not both: the
  author refuses the mix at commit and a receiver rejects a bundle whose
  members mix intents and rows, since mergeable rows cannot drop with a
  replayed member. (Phase 8 removes `Mergeable`.)
- A bundle is ready when every member this replica holds is present
  (unchanged); held members replay together and drop together.
- After replaying held members of a transaction with several members, a
  replica with write rights in their partitions signs an outcome record
  (`OutcomeRecord`: transaction, each held member with the partition
  prefix it replayed after, `Applied` or `Dropped` with its reason, engine
  identity) and appends it as a frame to each of those members' logs.
  A replica stores the latest record per publisher. While a record that
  names a member it does not hold says the transaction dropped, its own
  members of it drop with that reason; a record that changes this rewinds
  and redoes them. Two records of the same members after the same prefixes
  that disagree are refused as divergence, naming both engines.
- The author publishes its record at commit, in every member's log; a
  rewind that redoes the transaction publishes again, superseding it.
- A seal covers a replayed transaction of several members only once
  records name every member (`secure_outcome_members`); otherwise it stays
  open and its drop provisional. Before sealing, `seal_followed` has the
  replica publish records for open transactions it holds whole that lack
  them (`publish_missing_outcomes`); transactions a finalized seal reached
  but that stay open are listed by `awaiting_outcomes`, and the routine
  seal pass reports them as `replayed_awaiting_outcomes` activity.
- Routine seals: the SDK sync pass has the owner seal the followed tip
  (`seal_followed`) once `unsealed_replayed_transactions()` reaches
  `ROUTINE_SEAL_TRANSACTIONS`; finalizing the seal ends those rewinds.
- Rewind cost (diagnostic, unoptimized test build, desktop): one arrival
  rewinding and redoing 32 single-row transactions took 6.2 ms, about
  0.19 ms per transaction. `ROUTINE_SEAL_TRANSACTIONS = 128` bounds a full
  rewind to about 25 ms here; assuming a phone ten times slower, about
  0.25 s. Not measured on a phone.

Tests: `rocky-secure` `tests::partitions::replayed::a_partial_holder_drops_its_member_when_a_full_holders_record_says_so`
(a writer's two-partition create conflicts in B with an earlier create; a
full holder drops it and publishes its record; a replica holding only A
applies its member, then drops it on the record; the writer rewinds on the
earlier create, drops the same way and accepts the record without
divergence), `tests::partitions::replayed::a_seal_covers_a_transaction_once_every_member_has_a_record`
(the author's commit record; a partial holder's seal leaves the
transaction open until the author's record names the member it lacks; a
lost record is published again by a replica holding the transaction
whole, once),
`tests::sql::intents::replayed_writes_commit_as_intents_and_never_beside_mergeable_rows`,
`tests::sql::intents::a_rewind_across_the_open_tip_redoes_every_transaction`.
Statement budgets unchanged: author 43, receive 31.

Gaps:

- A transaction whose partitions no replica with write rights holds
  whole, once its author is gone, keeps no complete records: it stays
  open and is reported as awaiting outcomes.
- Divergence refusal has no test with a forged disagreeing record.
- `awaiting_outcomes` and the routine seal are not exercised end to end
  through a real seal and the SDK.
- Outcome exposure through `rocky-sdk` and Kotlin is still open.
