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

## Slice 7: convergence tests

Status: done (2026-10-30), on base `oxtkzqzynsrs` (`4b5a4c9ac`).

`rocky-secure` `tests::partitions::replayed::convergence` checks receivers
against an independent reference: plain in-memory SQLite (`rusqlite`) runs
the statements carried in the intent frames, transactions in stamp order,
each in its own savepoint; a failing transaction rolls back in every
partition and counts as a `Constraint` drop. The reference asserts that the
partition orders agree on one transaction order. It has no rewind, no undo
and no records. Each receiver must match its rows and every transaction's
outcome (`transaction_outcome`).

- `every_schedule_of_four_transactions_converges_to_the_canonical_replay`
  (integration): two authors over partitions A and B. W1 creates `x` in A.
  W2 concurrently creates `x` in A and `y` in B in one transaction, which
  drops in both partitions. W2 raises `x` in every held partition, and W1
  doubles it. The delivery units are each step's new frames, intents and
  outcome records. The test covers every order of the units and every split
  into consecutive packets (4!·2³ = 192 schedules). Each schedule runs on a
  fresh receiver with one fault: half restart the receiver after a packet,
  half retransmit everything delivered so far.
- `every_schedule_of_five_transactions_converges_to_the_canonical_replay`
  (e2e): adds a fifth unit. W1 receives W2's transactions, rewinds,
  publishes its records and recreates `y` in B. 5!·2⁴ = 1,920 schedules.
- `seeded_schedules_of_thirty_two_transactions_over_four_authors_converge`
  (integration, seeds 0–1) and `seeded_schedule_sweep_converges` (e2e, seeds
  2–15): four authors commit 32 random transactions: creates (some
  across both partitions, some colliding), increments in one or every
  partition, assignments, doublings and deletes. Before each commit the
  author receives a random set of the others' steps, closed under causal
  dependencies and whole bundles. Two receivers get every frame in a
  random order and packet split, one with a restart and one with a
  retransmission. Then every author receives what it lacks. Every replica
  agrees with the reference.

The tests found five engine defects, fixed in this slice:

- **Replayed ids never advanced.** `next_id` reads identity marks, which
  only mergeable row frames raised, so a second insert reused an id. The
  author's commit and every replay now raise the mark for each row they
  create, inside their transaction. An id of another author's number is
  refused at commit and dropped as `Admission` on replay
  (`replayed_inserts_take_their_authors_own_identities`).
- **Updates to equal values dropped as `Admission`.** A captured row whose
  images are equal needs no right, but it was admitted as a change with
  none, so both the author and replayers refused it. Whether an update
  changes anything depends on the canonical state, so this split outcomes.
  Such rows are now skipped (`an_update_to_equal_values_needs_no_right`).
- **Outcome records grew with every rewind.** Each redo published a new
  record, because the prefix had moved. One seed reached 3,640 frames in
  a sync. A replica now publishes again only when its outcome for the
  members changes, as the settled text says.
- **A bundle with an unopenable held member was rejected.** A settled or
  pending sibling whose key was not installed yet rejected the whole
  bundle for good. It now waits (`OpeningKey`), as a single frame does.
- **A rewind needing a missing key rejected the arrival.** When a
  rewound transaction has a member in a partition whose key is not
  installed yet, the redo now fails with `KEY_WAITS`, the savepoint rolls
  the rewind back, and the arrival waits for the key. This covers bundles
  and outcome records
  (`a_rewind_waits_for_its_partition_keys_and_records_only_changed_outcomes`).

Timing (diagnostic, debug build, Apple M5 Pro, 18 cores, load 2–9 from
other agents, `cargo nextest run -p rocky-secure`, one run each): four
transactions 8.8–16 s, five transactions 100 s, seeded 2 seeds 3–16 s, sweep
13 s. Schedules run on up to 8 threads but do not scale: each receiver open
performs `F_FULLFSYNC`s, which serialize across the device. Opening a
copied template receiver was no faster. Durability is verified at open, so
the tests do not relax it. The e2e tests have a nextest override
(`.config/nextest.toml`).

Limits: receivers hold both partitions. Partial holders are covered by
the slice 6 tests, not by the exhaustive schedules. Only `Constraint` drops
occur in the reference. `Admission` and `Schema` drops have their own tests.

## Slice 8: crash tests

Status: done (2026-10-30).

New qualification crash points (`CrashPoint::REPLAY`): `Apply` (a replayed
transaction, before its receive commits), `Rewind` (after each undone
member), `Redo` (between redone transactions) and `UndoDeletion` (after a
seal marks covered transactions sealed and drops their undo records). Like
the existing points, each fails its step inside the transaction. The test
then drops the replica without closing it. `crash_at` now takes `&self`, so
the replay code can reach it.

`tests::partitions::replayed::convergence::crashes` uses the five-unit
fixture. Each test reaches its point, checks that it fired, and drops and
reopens the receiver. It then checks that the receiver equals the
reference replay of exactly the frames it was given. Received frames were
kept before the interrupted step, so the reopen finishes it whole. Then
every frame is delivered again, the result must equal the full
reference, and both partitions seal:

- `a_crash_while_applying_an_intent_reopens_and_converges`
- `a_crash_inside_a_rewind_reopens_and_converges` (W1's doubling applied
  first; W1's earlier create rewinds it)
- `a_crash_between_redone_transactions_reopens_and_converges` (three
  transactions redo)
- `a_crash_while_deleting_undo_records_reopens_and_converges` (the seal
  did not happen: the unsealed count is unchanged after reopen)
- `a_crash_right_after_a_rewinding_receive_commits_reopens_and_converges`
  (the existing `GroupAfterCommit` point)

Each test runs in under 0.5 s (integration tier).

Limit: undo deletion is reached through `seal_replayed_for_test`, inside one
transaction as `finalize_seal_impl` runs it. A real seal finalization of
replayed tables is not driven end to end.

## Outcomes in the SDK and Kotlin; install refusal lifted

Status: done (2026-10-30). This closes the slice 3–6 gaps on outcome
exposure.

- The secure profile installs replayed tables. The `rocky-core`
  `intent-replay` feature is gone, and the trusted profile still refuses
  them. The Rust SDK declares them with `Table::replayed`, Kotlin with
  `TableClass.Replayed`.
- `Transaction::commit` returns `Option<TransactionToken>`, the
  transaction the commit published (`Connection::committed` for drivers).
  `Database::transaction_outcome(token)` returns `Applied` or
  `Dropped { reason, sealed }`.
- Observers: `TableChange.dropped` lists the transactions a committed
  replay dropped. It reaches every observer after the commit that records
  the drop (`rocky-storage` `TableEvents::publish_drops`). A rewind now
  marks the tables it reverts as changed; before, undo changed rows
  silently.
- Kotlin and uniffi: `NativeConnection.committed()`,
  `SQLiteConnection.committedTransaction()`,
  `RockyDatabase.transactionOutcome(token)` (`RockyTransactionOutcome`) and
  `NativeTableChange.dropped`.

Tests: `rocky-sdk` `tests/replayed.rs`
`replayed_counters_keep_both_increments_and_report_drops` (two devices
over the memory hub: concurrent increments both apply; a concurrent
duplicate create drops on both devices as `Constraint`, and its author's
observer receives its token). `rocky-storage`
`observe::tests::every_observer_receives_dropped_transactions_once`.
`rocky-sqlite` `aReplayedCommitReportsItsOutcome`.

## Late transactions in the successor; real seal finalization

Status: done (2026-10-30), on base `oxtkzqzynsrs` (`4b5a4c9ac`). This closes
the slice 5 gap and the slice 6/8 limits on divergence and undo deletion.

What the open item described did not match the code: under a `Continue`
seal a late frame already reaches `Live` in the successor (only `Close`,
losing and abandoned lines are `Held`), and a schema switch holds only
unoffered frames whose row writes touch removed fields, which intent frames
have none of. So line admission, held tokens and re-authoring needed no
change. The defects were in seal finalization and schema switches:

- **Finalization lost or misordered replayed rows.** Finalization moves the
  operations a seal excludes after the ones it includes, then restores rows
  from row records. Replayed scopes keep none: the full rebuild emptied the
  table (the test oracle reported "incremental finalization differs from
  full rebuild: items"), and the incremental path left late transactions in
  stamp order among the seal's members. Replayed scopes now skip the
  record-based restore, drops and tail.
- **Order is `(era, stamp)`.** `secure_replayed` gains `era`: the domain's
  finalized seal depth when the member joined (`OPEN_ERA`, bound inside the
  existing insert, so authoring stays at 43 statements). When a seal at
  depth `d` finalizes, `order_behind_seal` rewinds the unsealed members it
  excludes and everything after them, moves the excluded ones to era `d`
  and redoes all in `(era, stamp)` order: the seal's members first, then the
  late ones with their original frames, identity and stamp. Then
  `seal_replayed` marks the covered ones sealed and drops their undo. All
  inside the finalization transaction; outcomes may change (a late create
  drops behind a sealed one). Rewinds, outcome-record prefixes and arrivals
  compare `(era, clock, author, sequence)`, so a later arrival from an old
  tip also orders after the seal's members.
- **Schema switches of replayed tables.** The projection plan only replaced
  indexes of non-mergeable tables, so a changed replayed table kept its old
  columns (`InvalidParameterCount(6, 5)` at the switch), and the constrained
  path reprojected it from (absent) records. `rebuild_replayed` now copies
  the live rows' common columns into the new table, inside the switch
  transaction; `reshape_undo` rewrites unsealed undo images into the new
  column order (new columns take their default) in the plan, constrained
  and in-place switches, so a rewind across a switch restores rows the new
  table holds.
- **`Schema` drops.** A replayed statement that SQLite no longer compiles
  (for example a removed column) dropped as `Engine`; when it fails and no
  longer compiles (`describe`), it now drops as `Schema`.
- **Divergence is reported.** A refused disagreeing record was only marked
  rejected; its reason was lost. It is now kept in
  `secure_outcome_divergence` with both engines and the publisher, logged,
  and listed by `EncryptedReplica::outcome_divergences()`.

Format: `secure_replayed.era` and `secure_outcome_divergence` stay in this
phase's single replica bump (v40, unreleased); databases made by earlier
phase 4 builds are recreated.

Tests:

- `tests::partitions::replayed::convergence::successor::offline_peers_transactions_replay_after_the_seal_in_its_successor`:
  the owner creates `x`; offline from it, A raises `x` by 1, B (having seen
  that) raises it by 10 and creates `k`, and A receives B's. The owner
  doubles `x`, creates its own `k`, seals and finalizes. A reconnects,
  follows the successor and finalizes: rows and every outcome equal the
  reference replay of the seal's members, then the rest
  (`canonical_sealed`), `x = 13`; B's `k`, applied at A before, drops as
  `Constraint`. A authors no intent frame and holds nothing. The owner
  receives B's original frames from A twice (B never reconnects) and agrees;
  A agrees after a restart. The owner's next seal makes the drop final, and
  B, back at last, converges after finalizing both seals without authoring.
- `…::every_order_around_the_successor_converges_to_the_sealed_replay`:
  every order of the owner's and the three offline transactions, with the
  successor learned before, between or after them (24 · 5 = 120 fresh
  receivers), equals the reference after finalization.
- `…::a_crash_while_a_seal_{deletes_undo_records,rewinds_its_late_transactions,redoes_its_late_transactions}_reopens_and_converges`:
  the receive completing the seal's members finalizes it in the maintenance
  step after its group commit; the armed point fires there. Reopened, the
  seal is not finalized, every transaction is open in stamp order (equal to
  the provisional reference) with its undo; finalizing again yields the
  sealed reference and leaves undo only for the three late transactions.
  `UndoDeletion` is now reached through `finalize_seal_impl`, not only the
  test helper.
- `tests::generations::replayed::a_late_write_of_a_removed_column_drops_as_schema_in_the_successor`
  (replayed lineage, generation 3 → 4 removes `note`): carol writes `note`
  and then `title` offline; after the switch and finalization both replicas
  hold `title = 'kept'`, carol's `note` transaction dropped as `Schema` in
  era 1 and the `title` one applied, nothing held.
- `tests::partitions::replayed::a_disagreeing_outcome_record_is_refused_as_divergence_naming_both_engines`:
  a replica re-signs its record of a two-partition transaction as dropped
  under engine `forged engine` (`forge_outcome_for_test`). A receiver holding
  the author's and the replica's honest records rejects the forged frame,
  reports it with the stored engine (`rocky-secure … sqlite …`), the refused
  one and the publisher, and keeps `Applied` and both rows.
- `tests::sql::intents::a_thirty_two_row_intent_receives_within_its_statements_per_row`:
  receiving one transaction that changed 32 replayed rows costs 4.65625
  statements per row when each row has its own `UPDATE` and 3.6875 when one
  `UPDATE` changes all (gate ≤ 6.5, features_11 6.40625). The counter sees
  Rocky's statements; the test adds the replayed application statements,
  which run through `session_run` outside it.

Statement budgets after these changes (whole call / transaction or
application window): author 81 / 43 (≤ 43), single-row receive 86 / 32
(≤ 45; the arrival's era read adds one).

## Slice 9: desktop measurements

Status: L0, L2 and the 32-row gate measured and paired; the L0 target is
missed by the baseline as well on this host; replayed writes regress under
L2. The 100k member pull and the shopping app are recorded below.

The features_12 write probe and its L0–L4 load scenarios were removed in
`19823fe0d`. `rocky-sdk` `tests/write_latency.rs` restores L0 and L2 only
(both `#[ignore]`, release only), on a replayed (`counters`) and a
mergeable (`items`) table of the same shape, single-row `UPDATE …
SET amount=amount+1` through `Database::transaction` (buffered), 50 warm-up
and 600 samples each; it also reports the local observer notification
after `commit` returns. The probe was rerun on 2026-10-30 after the
implementation and workspace checks completed.

- L0: one device, a write every 10 ms.
- L2: as features_12 phase 1: a peer over the memory hub authors one small
  write to the same table every 20 ms while the probed device syncs in a
  loop; a probed write every 100 ms (3,246 peer writes per table).

Pairing: the phase-start commit `prpmqutmoxkz` (`2538c1db0`, interactive
first; slice 1 follows it) was exported with `jj file show` to
`/tmp/rocky-p4-base` and run with the same file, `counters` declared
mergeable there (replayed tables do not exist at that commit). Change:
working copy `wnmuwymwmtwu` on `oxtkzqzynsrs` (`4b5a4c9ac`). Host: Apple M5
Pro (Mac17,8), 18 cores, 48 GiB, macOS 27.0.1, other agents running (load
3.0–4.1 during the runs). Release build. Command, alternating current and
baseline twice (`/tmp/p4-measure/run.sh`):

```sh
cargo test --release -p rocky-sdk --test write_latency --no-run
target/release/deps/write_latency-<hash> --ignored --nocapture --test-threads=1
```

Write latency, p50 / p95 / p99 / max (ms), 600 samples each:

| Load | Table | Current round 1 | Current round 2 | Baseline round 1 | Baseline round 2 | Current round 3 |
| --- | --- | --- | --- | --- | --- | --- |
| L0 | `counters` (replayed; baseline mergeable) | 3.03 / 6.04 / 15.59 / 23.15 | 3.24 / 5.82 / 6.63 / 9.36 | 3.10 / 6.08 / 7.39 / 14.42 | 3.18 / 5.55 / 7.38 / 8.37 | 3.07 / 4.80 / 7.19 / 7.82 |
| L0 | `items` (mergeable) | 2.78 / 5.52 / 6.73 / 22.30 | 3.14 / 5.87 / 6.83 / 20.19 | 3.16 / 5.69 / 6.91 / 15.60 | 3.11 / 4.82 / 6.16 / 14.32 | 3.03 / 5.48 / 6.52 / 15.65 |
| L2 | `counters` (replayed; baseline mergeable) | 6.96 / 11.47 / 12.08 / 13.83 | 7.09 / 11.83 / 12.74 / 16.89 | 2.45 / 3.13 / 7.51 / 11.78 | 2.47 / 3.38 / 9.61 / 14.74 | 7.38 / 11.64 / 12.48 / 14.65 |
| L2 | `items` (mergeable) | 2.57 / 3.46 / 5.92 / 12.28 | 2.47 / 3.33 / 5.62 / 13.21 | 2.46 / 3.09 / 6.33 / 10.25 | 2.74 / 3.69 / 7.05 / 8.62 | 2.61 / 3.64 / 6.07 / 17.15 |

The observer notification arrived within 0.04 ms of `commit` returning in
every run (p95 0.00 ms).

- **L0 gate (p95 ≤ 3 ms) not met**, by replayed (4.8–6.0 ms) and by the
  baseline's mergeable writes alike (4.8–6.1 ms): on this host the idle
  write costs the same with and without replay. An earlier unpaired run
  (slice 9's first, load 12–15) measured 2.78 ms for replayed and 5.50 ms
  for mergeable writes. The quiet-host qualification remains open; this
  third paired run does not justify changing the gate.
- **L2 regression for replayed writes**: p50 about 7 ms and p95 about
  11.7 ms against 3.1–3.7 ms for mergeable writes in both trees, while
  receiving the peer's intents. Still within the L1–L4 desktop target
  (≤ 16 / 50 / 100 ms), but a paired regression. Not profiled; likely
  causes are replaying each received intent (and any rewind of the
  probe's own later writes) under the replica lock the next probe write
  waits for, and routine seals every 128 transactions.

Sync non-regression, desktop, 100k member pull (mergeable shopping
fixture, as features_11/12). `rocky-sdk` `tests/migrations/prepared_pull.rs`
`member_pull_of_100000_rows` (`#[ignore]`, release) prepares the 100k
migration fixture once and pulls it; the same file ran in both trees.
Command: `target/release/deps/migrations-<hash> --exact
prepared_pull::member_pull_of_100000_rows --ignored --nocapture
--test-threads=1` (`/tmp/p4-measure/pull.sh`), current then baseline, load
2.0–5.9, same host and change as above. One trial each:

| Measure | Current | Phase start `2538c1db0` | Ratio |
| --- | --- | --- | --- |
| Member pull, 100k rows | 9.96 s | 10.30 s | 0.97 (≤ 1.10) |
| Member open | 38.0 ms | 37.2 ms | |
| Member wire bytes (open + pull) | 44,770,553 | 44,795,315 | |
| Issuer file after migration | 111.4 MB | 111.4 MB | (≤ 300 MB) |
| Issuer seed open / fill open / publication / closing compaction | 74.0 / 84.4 / 55.2 / 29.5 s | 72.7 / 84.7 / 57.2 / 30.3 s | |

After the timed pull, both trees fail the harness's own transfer
accounting assertion (`tests/migrations.rs:647`), which the 8-row fixture
satisfies; so the second and third trials did not run and the member file
size was not printed. The failure is identical at the phase start, so it is
a harness assumption about the 100k fixture, not a phase 4 change. The
32-row gate is recorded above (4.66 statements per row). Phone rows are
phase 9.

## Shopping app

The shopping app's trusted-only `devices` table is declared
`TableClass.Replayed` (`TrustedRows.kt`). Its registration is a bound
`UPDATE`, then an `INSERT` with `nextId` when nothing matched, so it replays
as written; concurrent registrations of one node keep the first. The list
group (`lists`, `categories`, `items`, `item_categories`) stays mergeable:
its tables reference each other, a foreign key between classes is refused,
`items` uses a `LatestWins` policy and versions 7 and 8 migrate it, which
are phase 5 and phase 7 concerns.

## Checkpoints

A checkpoint holds row records, which replayed partitions keep none of, so
`prune` refuses replayed scopes (`PruneBlock::NoCheckpoint`) and their
frames stay; a member joining after compaction replays them
(`…::successor::compaction_prunes_no_replayed_frames_and_a_later_member_replays_them`,
`rocky-sdk` `a_member_joining_later_receives_replayed_rows`). Checkpoints
carrying replayed rows belong with phase 5's replayed row record.

## Checks (2026-10-30)

On working copy `wnmuwymwmtwu`, same host:

- `./scripts/lint` (after `--fix`): passed.
- `./scripts/check`: passed, 236 unit and 1,509 integration tests.
- `cargo test --release -p rocky-sdk --test write_latency --no-run` and the
  ignored L0/L2 probe: passed; 600 samples per table. The new paired results
  are included in the Slice 9 table above.
- `cargo check -p rocky-secure -p rocky-sdk --features rocky-sdk/qualification --tests`: passed.
- `cargo nextest run -p rocky-secure` (default profile, before the last
  edits): 647 of 648 passed; the e2e-tier
  `collects_every_entry_across_the_real_hash_window_before_recovery_is_signed`
  hit the default 60 s timeout, as e2e tests do outside their tier.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed.
- `./examples/shoppinglist-app/check`: compiles every module and passes the
  Android tests after republishing the SDK to Maven local; Amper had kept
  stale extracted AARs beside it (removed so it re-extracts). The JVM suite
  failed one test per run, a different one each time
  (`SchemaUpgradeTest` version 6 member reconcile; `SealsTest` custody,
  offline reopen, branch), equally with `devices` mergeable (each class run
  alone, twice per configuration), so the failures are timing flakiness,
  not this change.
- Not verified: iOS (`check-ios`), phones, the shopping multi-process
  qualification.

## Open

- **L0 target.** Idle replayed writes measure p95 5.8–6.0 ms against
  ≤ 3 ms; the phase-start mergeable writes miss it alike on this host
  (4.8–6.1 ms). A quiet-host run is needed to decide the gate.
- **L2 regression.** Replayed writes under L2 are about 3.5× slower at p95
  than mergeable ones (11.7 vs 3.4 ms), within the L1–L4 target but
  unprofiled.
- **Member pull trials.** One paired trial each; the harness's transfer
  accounting assertion for the 100k fixture fails identically in both
  trees, so repeated trials and the member file size are missing.
- Phones, L1, L3 and L4 are not run (phase 9).
