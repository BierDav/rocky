# 9. Qualification: evidence

Work on change `soosykzp` (parent `aeae8744`, "improve todo app example"),
2026-11-02. Host: Apple M5 Pro (Mac17,8), 18 cores, 48 GiB, macOS 27.0.1,
rustc 1.99.0. Other agents shared the host. Load average was 5–30 during
the runs, so timings are diagnostic. The todo app replaces the deleted
shopping app throughout.

## Devices and builds

- **Not verified: every physical-device row.** The iPhone 15
  (`david-iphone`) was listed `unavailable` by `xcrun devicectl`. No
  Android device was attached (`adb devices` empty). No second desktop
  was on the LAN. No mobile build of the todo app or of a write probe
  exists: the Kotlin SDK, UniFFI bindings and shopping app were deleted
  (2026-11-01), and `todo-desktop` is a Dioxus desktop app. Slices 1–6 on
  real devices therefore did not run. Desktop numbers are not substituted
  for phone rows.
- **Build identity, macOS arm64, release:** `target/release/todo-desktop`
  SHA-256
  `cdeb10fa71c2c4484243fa0f4e3beebffc2e5f995212f7ae22510ec4876ebba6`.
  There is no features_13-start build of this app to pair with. At
  `469fc97e` the todo app ran on the operation model of `rocky-runtime`, now
  `examples/runtime-todo-app`.
- **No paired desktop probe exists.** The start commit's probe is
  `tests/migrations/write_probe.rs` (L0–L4, register path). The current one
  is `tests/write_latency.rs` (L0/L2). Its 100k fixture is mergeable; the
  current one is replayed. So the desktop rows below are current-tree
  results. Pairings already recorded by earlier phases are cited where
  they exist.

## Slices 3–6: convergence on the todo app

The device scenarios run in process over the memory transport in
`examples/todo-app/tests/qualification.rs` (integration tier). Shared
helpers are in `tests/common/mod.rs`. Each scenario asserts that every
device has the same rows (`Todos::all_todos`) and the same outcome for
every transaction token (the drop map).

- **Slice 3**
  (`three_devices_editing_one_list_converge_at_the_next_seal`): laptop
  (trusted), phone and tablet (full) edit one list at the same time,
  using `tokio::join!` with no sync in between:
  - two moves into the gap after "Post";
  - three raises of one priority;
  - a rename;
  - two adds of "Pharmacy".

  After the next seal, all three have identical rows and drop maps:
  - both moves keep their place;
  - priority is 3;
  - exactly one "Pharmacy" add is `Dropped { Constraint, sealed: true }`
    on every device;
  - the author of the dropped add receives it in `TableChange::dropped`.

  A compaction at the seal and a fourth device joining afterwards keep the
  same rows. The concurrent 100k phone bootstrap was not run (no phone).
  Phase 10 trigger records: not observable in process.
- **Slice 4**
  (`offline_edits_across_a_routine_seal_replay_in_the_successor_tip`): the
  phone goes offline (`set_networking_enabled(false)`) and writes "Milk", a
  raise and "Bread". The laptop writes `ROUTINE_SEAL_TRANSACTIONS` (128) adds
  plus its own "Bread", and its routine seal opens a successor tip. When
  the phone reconnects, its late "Milk" and raise stand in the successor.
  Its "Bread", ordered after the sealed one, is the only drop:
  `Constraint`, provisional, then final at the next seal, the same on both
  devices.
- **Slice 5** (`a_killed_app_reopens_offline_with_whole_transactions`): a
  child process commits one buffered write and calls `abort()` right after
  `commit()` returns. On offline reopen, the write is present. A second
  child commits two-todo transactions in a loop and is `SIGKILL`ed after
  reporting 40. On reopen, every pair is whole and every reported pair is
  present.
- **Slice 6**
  (`the_open_tip_grows_while_no_owner_is_online_and_converges`): with the
  only owner offline, phone and tablet grow the open tip to 512
  transactions (4 × 128). Each round, each side writes 16 transactions and
  then they sync, so arrivals interleave and rewind. Every 4th transaction
  raises one shared priority. The members converge with no drops. Once the
  owner returns and seals, all three hold the same rows with priority 128.
  Rewind cost (debug build, loaded host): 16 member sync rounds, median
  250–261 ms, max 369–439 ms. Owner catch-up and seal: printed per run,
  under 1 s.

## Defects found and fixed

Slices 3, 4 and 6 first failed: after any seal, members lost every todo,
and their drops never became final. Fixed with tests that fail without
each fix:

1. **The todo app could not seal.** Rocky defines no bookmark, and the
   creator followed none, so no device sealed, routinely or otherwise.
   `roles::create` now creates and follows `main` (`roles::BOOKMARK`).
2. **A checkpoint import made a neighbouring scope look gapped**
   (`rocky-secure`). `supersede` deleted the frames a base covers, and
   their author positions went with them. A scope the author never wrote
   (the `lists` partition, written only by the laptop) then counted the
   phone's and tablet's first positions (their `devices` registrations) as
   missing history. Its seal rebootstrap reset it, to replay from peers.
   - Fix: a superseded frame's signed header is kept as a placement
     (`place_superseded`).
   - Fix: a placement in a held scope now counts unless that scope is the
     one being checked (`held_positions` and its test oracle).
   - Test:
     `tests::compaction::anchors::superseded_frames_stay_placed_for_scopes_their_writer_never_wrote`.
   - Effect: in the todo scenarios, seal rebootstrap resets dropped from 4
     to 0.
3. **A parent's base took other scopes' rows** (`rocky-secure`).
   Installing a checkpoint ran `DELETE` on the scope's rows and then
   inserted the base. `todos … ON DELETE CASCADE` deleted every todo of the
   list, in the `todos` scopes, which nothing restored unless their own
   import happened to follow.
   - Fix: `replace_replayed` collects the rows that delete actions reach,
     transitively, before clearing. After the base is installed, it puts
     back each one whose parent row is back. Rows whose parent the base
     lacks stay as the action left them.
   - Test:
     `tests::replayed_constraints::a_base_of_a_parent_scope_keeps_the_children_of_rows_it_keeps`.
4. **Members finalized seals only on open or after a local write**
   (`rocky-sdk`). Maintenance finalizes only for scopes a local SQL commit
   queued, and `finalize_path` ran only at open. A member that received a
   seal kept reporting `sealed: false`.
   - Fix: each recovery pass now calls `EncryptedReplica::finalize_arrived`.
     It finalizes the held seals of the followed path and publishes a
     revision only when one finalized.
   - Test: `rocky-sdk` `replayed::a_member_holding_the_seal_reports_its_drop_final`.
5. **A rewind across a schema switch restored a removed field**
   (`rocky-secure`, pre-existing). This one surfaced through fix 4. A
   member that switches from A straight to C, past a B that removed
   `note`, kept undo images captured at A. `reshape_undo` mapped them by
   column name, and A and C share the name, so finalizing seal A rewound
   the member's late write and restored "old".
   - Reproduced on the unchanged code by reopening the member, which
     finalizes at open.
   - Fix: `reshape_undo` takes the switch's path fields and defaults every
     column or table that some tip on the path lacked.
   - Test: `removal::a_readded_field_never_receives_a_value_written_before_its_removal`,
     which now finalizes in the background.

### Open

- **The frames-reset path still cascades.** `replay_scope(reset = true)`,
  run when a seal rebootstrap has to replay a scope's pruned frames from
  peers, clears the scope's rows. Delete actions reach rows of other
  scopes, which replay nothing. After fix 2 the todo scenarios never take
  this path, and a legitimate one needs pruned frames that a peer still
  retains. The fix is a design choice for the phase 5/8 owners. Either
  reset the reached scopes with it, so they replay their own frames, or
  defer the clear until the frames are back.
- **Phase 10 is not triggered by this evidence.** On desktop, no write
  exceeded the max targets: the L2 max was 44.6 ms, with replay and
  rewinds of the peer's interleaved intents and routine seals running.
  Phone rows were not measured, so the phone side of the trigger is not
  evaluated. The brief still names "the shopping app scenario"; the todo
  app replaces it.

## Desktop measurements (release)

Write latency: `cargo test --release -p rocky-sdk --test write_latency --
--ignored --nocapture --test-threads=1`. 600 samples per table after
warm-up, load average 9–19. Values are p50 / p95 / p99 / max in ms.

| Load | Table | Write | Observer after commit |
| --- | --- | --- | --- |
| L0 | `counters` (replayed) | 3.50 / 6.11 / 11.15 / 17.59 | p95 0.00, max 0.01 |
| L0 | `items` (replayed) | 3.27 / 6.33 / 9.51 / 17.95 | p95 0.00, max 0.04 |
| L2 | `counters` (replayed) | 8.05 / 16.70 / 33.20 / 44.57 | p95 0.00, max 0.03 |
| L2 | `items` (replayed) | 8.31 / 16.67 / 24.94 / 37.98 | p95 0.00, max 0.01 |

Under L2, the peer authored 3,244 writes while 5,161 syncs ran. L1, L3 and
L4 have no current harness: `write_probe` was removed with the register
path. They are not verified.

100k replayed fill and member pull: `cargo test --release -p rocky-sdk
--test migrations replayed::fill_of_100000_replayed_rows -- --ignored
--nocapture --exact`. Two runs of three trials, load 5–15:

| Measure | Six trials | Phase 8 Session 6 (same fixture) |
| --- | --- | --- |
| Issuer fill open | 5.63–6.06 s | 5.68–5.81 s |
| Member pull | 11.21–11.71 s | 11.30–11.83 s (phase-8 start: 10.76–10.86 s) |
| Issuer file | 52.2–52.8 MB | 52.8–52.9 MB |
| Member file | 61.3–61.4 MB | 61.4 MB |
| Peak WAL seed / fill / pull | 52.6 / 62.9 / 43.9 MB max | 52.4 / 62.9 / 43.9 MB max |

The fixes did not move the fill or pull. Against phase 8's start build,
member pull stays at 1.04–1.09×. There is no quiet host and no pairing with
the features_13 start, whose fixture is mergeable.

Statement budgets (`cargo test -p rocky-secure --lib tests::sql::intents`,
debug):
- author: 79 total, 43 per transaction;
- single-row receive: 86 total, 32 per application;
- 32-row receive: 4.65625 per row each, 3.6875 overall.

## Goals

| Measure | Baseline | Target | Result | Pass |
| --- | --- | --- | --- | --- |
| Replicas at one seal: rows and drop list identical | registers converge rows only | identical under every tested schedule | identical in slices 3, 4 and 6 (in process), after fixes 1–5; exhaustive schedules in `rocky-secure` (phases 4, 5) pass | pass in process; devices not verified |
| Concurrent `x = x + 1` | one lost | both applied | 3 raises → 3 (slice 3), 128 → 128 (slice 6) | pass |
| Interactive write L1–L4, desktop (p95 / p99 / max) | features_12 phase 1 | ≤ 16 / 50 / 100 ms | L2: 16.7 / 33.2 / 44.6 ms; L1, L3, L4 no harness | L2 miss at p95 (16.70 > 16) under host load 9–19, p99 and max pass; others not verified |
| Interactive write L1–L4, phone | features_12 phase 1 | ≤ 33 / 100 / 250 ms | — | not verified (no phone) |
| Interactive write idle, desktop p95 | features_12 phase 1 | ≤ 3 ms | 6.11 / 6.33 ms | miss, as in phases 1 and 4; the paired baseline is no faster (phase 4) |
| Interactive write idle, phone p95 | features_12 phase 1 | ≤ 10 ms | — | not verified |
| Observer notification p95 | features_12 phase 1 | ≤ 5 ms | 0.00 ms | pass |
| Single-row intent receive statements | 83 | ≤ 45 | 32 | pass |
| Single-row intent author statements | 87 | ≤ 43 | 43 | pass |

The other goals rows belong to phases 5–8 and are recorded in their
evidence.

### Sync non-regression

| Measure | features_11 | Allowed | Result | Pass |
| --- | --- | --- | --- | --- |
| 32-row receive, statements per row | 6.40625 | ≤ 6.5 | 4.65625 | pass |
| Desktop 100k member pull | 15.2 s (paired) | ≤ 110% of paired | 11.2–11.7 s; 1.04–1.09× the phase-8 start (Session 6) | pass against phase 8 start; not paired with the features_13 start |
| Phone 100k bootstrap, LAN | 16.6 s | ≤ 18.3 s | — | not verified (no phone) |
| One-row sync phone ↔ desktop, LAN, p95 | 128.6 ms | ≤ 150 ms | — | not verified (no phone) |
| Empty phone sync | 0.83–1.05 s | ≤ 1.15 s | — | not verified (no phone) |
| Member file after 100k sync | 65.1 MB (phone) | ≤ 80 MB | 61.4 MB (desktop member) | desktop pass; phone not verified |
| Issuer file after 100k migration | 125.3 MB | ≤ 300 MB | 52.2–52.8 MB | pass |

## Checks

- `cargo nextest run -p rocky-secure -p rocky-sdk -p todo-app` (default
  profile, load 10–30): 868 run, 853 passed, 13 timed out at 60 s, and 2
  failed.
  - The 13 timeouts all pass alone under the e2e profile.
  - `devices_converge_over_iroh` is the known iroh flake (phase 8).
  - `removal::a_readded_field…` led to fix 5 and passes now.
- `cargo nextest run -p rocky-sdk --test removal`: 6 of 6 passed.
- `cargo test -p todo-app`: 3 unit, 5 qualification and 2 replica tests
  passed.
- `./scripts/lint` passes. The first `./scripts/check` failed on three
  errors in `todo-desktop`, all already in the parent change: an unbounded
  `for` range and a manual `is_multiple_of` (clippy), and a test that used
  `tempfile` without depending on it. With those fixed, `./scripts/check`
  passes: 229 unit and 1,290 integration tests.

## Post-qualification fixes

- **Scope resets took other scopes' rows** (`rocky-secure`, closes the
  open "frames-reset path still cascades" item). All four replayed
  scope-reset paths were affected: `replace_replayed` (checkpoint import),
  `replay_scope(reset = true)` (seal rebootstrap from frames),
  `abort_direct` (aborted direct import) and eviction (`clear_typed`).
  SQLite runs delete actions inside the transaction and cannot switch
  foreign keys off there.
  - Fix: one helper, `clear_replayed(scope)`. When the clear changes any
    row outside the scope (the connection's total changes exceed the
    scope's row count), the scope is marked (`secure_replayed_rederive`).
    `rederive_replayed` then rebuilds the domain's replayed rows from
    bases and frames once no marked scope waits for a seal rebootstrap,
    pending frames, an eviction or a direct import. The rebuild runs in a
    savepoint. Reinstate is deleted.
  - A mark is retried wherever its wait can end: at the end of
    `switch_to_staged` and `settle_seal_rebootstrap`, at the end of
    `settle`, in `recover_locally` (`EncryptedReplica::rederive`), at
    the last eviction step, and before every finalization pass
    (`finalize_ready`/`finalize_at_most`, so maintenance, `finalize_arrived`
    and `compact` retry too). Every caller holds the replica as background
    work, so like a rewind a rederive starts only when no interactive
    waiter is registered.
  - While a mark is pending, no seal finalizes, `compact` writes no base,
    and multi-member outcome records are not published. The rebuild
    unmarks inside its savepoint before replaying, so it publishes what
    it decides. `CompactionStatus::waiting`, `CompactReport::waiting` and
    the `FinalizationWaiting` blocker name the reason.
  - A rebuild that leaves a foreign key violated rolls back. It is
    recorded in `secure_replayed_stuck` with a digest of its inputs:
    applied and pending frames, bases, finalized seals, subscriptions,
    scope count and followed tip. Until that digest changes, a retry only
    reports "cannot be derived again: a row of T references a missing row
    of P" and does not rebuild. A violation can come only from a restored
    base row whose parent the rebuild lacks. Replayed members check their
    keys at each transaction, so a member that references a row of an
    unheld scope drops (`constraint`), as on a fresh replica. A fresh
    replica cannot commit such a base either: `restore_images` defers
    foreign keys to the commit. This last point is inferred from the
    code, not tested.
  - A `KEY_WAITS` refusal inside the rebuild rolls it back and leaves the
    mark (it no longer aborts the import). `reset_replay_ledger` runs for
    every scope of the domain on purpose: the rebuild replays every
    scope, so every scope's replay ledger restarts from its base marks.
    Permanent identity marks are separate and untouched.
  - `abort_direct` reaches no rows: a direct import's scope holds nothing.
    So it no longer leaves a spurious mark.
  - Eviction: held rows that the evicted scope's delete actions changed
    are derived again against what the device still holds. They equal a
    fresh partial replica that never held the scope: a tag that references
    an item drops (`constraint`), with a `CASCADE` or a `SET NULL`
    reference alike.
- **Outcomes were lost on reset, and kept ones could be wrong**
  (`rocky-secure`).
  - Fix: `secure_kept_outcomes(hash, txn, scope, clock, dropped, sealed,
    violated, based)` keeps only members this node authored (its author
    number). Finality is what the record held, or what an installed
    base's sealed outcomes decided (`based`, below). Nothing is inferred
    from a frame's absence.
  - Precedence across members: final drop, final apply, unknown past a
    base's horizon (reads `None`), provisional drop, applied.
  - Pruning: kept rows shadowed by a live record go at each seal
    finalization (`seal_replayed`) and after a rederive. At most
    `KEPT_OUTCOMES` (4,096) stay, newest by clock. Older ones read as
    `None`. A lost compacted seal resets kept `sealed` and `based`.
  - Format: still `rocky.encrypted-replica.v43` (unreleased; layout
    changed in place).
- **A base's covered outcomes said neither applied nor dropped**
  (`rocky-secure`, `rocky-core`). `TransactionOutcome::Covered` is
  removed.
  - Fix: `CheckpointStatement.outcomes: SealedOutcomes` (checkpoint
    format 18, 19 since open members below). A replayed scope's base at a
    seal states every drop through the seal (transaction, member stamp,
    `DropReason`, violation name cut to 256 bytes). It is the newest
    `MAX_SEALED_OUTCOMES` (512) by stamp; `horizon` is the newest stamp
    left out. It is signed with the statement, like the rows' LtHash.
    `verify_signature` refuses a list out of stamp order, past the bound,
    with a horizon on a list that is not full, or with a violation on a
    reason other than `constraint`/`restricted`. Other scopes and
    no-frames bases state an empty list.
  - Taken at seal finalization with the seal rows
    (`secure_replayed_seals.outcomes`): the current base's drops and every
    sealed drop since. Compaction
    and served kept seals sign the seal's list; a kept seal without one is
    not served.
  - Import (`supersede` → `keep_covered`): an own covered member becomes
    `Dropped { sealed: true }` with the listed reason and violation, or
    `Applied { final: true }` when unlisted (`based = 1`; later records
    do not overwrite it). One whose sealed record contradicts the list
    refuses the import.
  - Still unknown: an own covered member that is unlisted and at or
    before a full list's horizon (more than 512 later drops or open
    transactions in the scope). It reads `None`, as an outcome past
    `KEPT_OUTCOMES` does.
  - Tests (`tests::compaction::cascades`), each failing with
    `keep_covered`'s lookup disabled:
    `an_outcome_a_base_covers_reports_the_seals_drop_never_its_stale_record`
    (replaces `…_reports_covered_…`; final `constraint` drop with A's
    violation), `an_applied_outcome_a_base_covers_is_final`,
    `replicas_at_one_seal_report_identical_outcomes` (A, B through A's
    bases, C replaying frames; A's and C's statements state the same
    list) and `a_forged_drop_list_is_refused_on_import` (altered unsigned,
    four malformed re-signed lists, a list contradicting a sealed own
    record). `an_authored_outcome_survives_imports_and_restarts_after_compaction`
    fails too. Frozen checkpoint vectors were regenerated with a dropped
    and an open entry; `verify.py` checks them.
- **`Applied` had no finality** (`rocky-core`, `rocky-secure`,
  `rocky-sdk`, R3). `TransactionOutcome::Applied { final }` (Rust field
  `sealed`), like `Dropped`: final once a seal covers the transaction,
  from the live record, a kept outcome or a covering base's list.
  `TableChange::sealed` names this installation's transactions that a
  committed seal finalization or checkpoint import may have made final.
  The SDK crate docs give the outbox pattern. Test:
  `replayed::an_applied_transaction_turns_final_with_the_seal_and_stays_final`
  (provisional before the seal on both devices, final after, observer
  notified, final after the author restarts). The import case is
  `an_applied_outcome_a_base_covers_is_final` and, with restarts,
  `an_authored_outcome_survives_imports_and_restarts_after_compaction`.
  - Final is relative to the followed seal path. When a concurrent seal
    of the same tip wins (lower digest), the losing seal's outcomes
    become provisional again until the winner finalizes. The rival-seal
    cascades tests now assert this. An application that acts on
    `final` therefore relies on owners not sealing one tip concurrently.
- **A base at a seal that left a member open lost it for good**
  (`rocky-secure`, closes the open-members item above). The seal's
  frontier covers a member of a transaction of several members whose
  other members' outcome records have not arrived. Seal rows undo it, an
  importer supersedes its frame, and that importer never applies it.
  - Reproduced by `tests::compaction::open_members::a_base_at_a_seal_that_left_a_member_open_converges_at_the_next_seal`:
    before the fix, Q, which imported P's base, lacked the row that A,
    F (frames) and P held at the next seal.
  - Fix: `leaves_open(scope, depth)`. A seal that covers an open member
    of the scope, or of a linked scope, leaves the rows there undecided.
    Finalization takes no seal rows for that scope, so the replica serves
    no kept seal and starts no rebuild there. `compact` writes no base
    for it, and a later seal that leaves nothing open does. The test now
    asserts that P offers no base at the first seal, and that a replica
    importing P's base at the next seal converges.
  - Why replicas then agree: a base at seal S comes only from a replica
    that decided every member S covers in the scope and its linked
    scopes. Those decisions are deterministic: same frames, same
    canonical order, same records. The rows are therefore the canonical
    replay of the covered members, and every uncovered member orders
    after them (open era). An importer supersedes only frames whose
    effects the rows hold and replays the rest, so its rows and drops
    equal a full replay at S and, by induction, at every later seal.
  - `SealedOutcome::Open` is gone. Checkpoint format 19,
    `rocky.encrypted-replica.v45`. Frozen vectors regenerated with two
    drops; `verify.py` checks them.
  - Not changed: a replica that seals members ordered after its own open
    member still seals them. If the open member later drops, rewind
    reaches only unsealed members. This is inferred from the code, not
    tested.
- **`finalize_arrived`** counts the unverified-scope pass only when a
  scope verified. It finalizes at most `ARRIVED_SEALS_PER_PASS` (4) seals
  per call. A rederive counts as one step.
- Tests (`tests::compaction::cascades`, two references from `tags` into
  `items`, a cascade and a `SET NULL`). The tests marked † fail when
  their fix is disabled (checked by reverting each):
  - `a_parent_replayed_from_frames_keeps_the_rows_that_reference_it`
  - `an_imported_parent_base_keeps_children_of_rows_applied_over_it`
  - `an_authored_outcome_survives_imports_and_restarts_after_compaction`
  - † `children_return_when_a_frame_that_waited_through_an_import_arrives`:
    an `items` frame waits for its predecessor through an import, then
    arrives by sync. Disabling every retry point fails it; with only the
    `settle` retry disabled, maintenance's finalization retry still
    restores the rows.
  - † `no_seal_finalizes_while_rows_wait_to_be_derived_again`
  - † `an_evicted_parent_leaves_what_a_replica_never_holding_it_replays`:
    rows and outcomes equal a fresh partial replica holding only `tags`.
  - † `an_outcome_a_base_covers_reports_the_seals_drop_never_its_stale_record`
    (renamed; see above): before the fix, B reported `Applied`, while
    A's seal dropped the transaction (`constraint`, final).
  - † `kept_outcomes_go_once_records_return_and_stay_bounded`
  - † `a_rebuild_that_cannot_hold_is_stuck_until_its_inputs_change`
  - † `a_cancelled_direct_import_reaches_no_rows_and_marks_nothing`
  - `an_outcome_survives_a_replay_from_frames_and_a_restart` guards the
    redesign; it is not a regression test of an old defect.
  - `a_partitioned_child_of_an_unpartitioned_parent_is_refused`: the
    requested fixture (`partitioned` plus `linked`) is refused by
    `rocky-core` (`CrossScopeReference`). Partitioned references need the
    same partition group, so no reference reaches across partitions into
    another table's scopes. A self-reference inside one partition is not
    exercised; its intra-scope cascade does not mark.
  - `scope_local::an_idle_finalization_pass_publishes_nothing`
- **Rederive cost (F5, measured, not bounded).** Change `yvwwmlup`
  (parent `07a2901d`) plus these edits, Apple M5 Pro, release build,
  load average 10–45 (shared host), one sample each, `cargo test
  --release -p rocky-secure --lib
  tests::compaction::cascades::rederive_of_100000_rows -- --ignored
  --nocapture`. Fixture: 100,000 `items` rows in 1,000 transactions.
  - Rederive from frames: 8.84 s.
  - Rederive from a base, after seal and compaction: 1.31 s.
  - Fill: 10.1 s.

  The whole rebuild holds the replica in one step without yielding.
  Bounding it is phase 10's question.

## Relational lifecycle fuzz

The SDK simulation gains a relational fixture
(`rocky-sdk` `simulation::relational`, seeds in `relational_tests`).
`boards` and `cards` are partitioned by `list` in one group. A card has a
`CASCADE` key to its board and a `SET NULL` pin to another board. Codes
and names are unique per list from small pools. `CHECK`s bound caps and
points, and the `board_cap` assertion bounds a board's points.

- **Actions:** concurrent inserts, updates, deletes and moves of boards
  and cards (moves and board-and-card pairs are two-member bundles);
  seals, compaction, invites after a compacted seal (checkpoint import),
  crashes at every `CrashPoint`, restarts, revocation, offline periods,
  eviction of a list by a partial member (the owner as its full node),
  holding it again, link faults.
- **After quiescence:** the owner seals and everyone converges at that
  seal, every member restarts and converges again. Then:
  - every holder of a list has the reference's rows;
  - every holder of all of a transaction's lists has its outcome
    (reason and `sealed`);
  - every author can still query its transactions;
  - no replica shows an orphan, a dangling pin, a failed check or a
    violated assertion.

  The replica audit and these row checks run after every step.
- **Reference:** a plain in-memory SQLite database with hand-written
  DDL. It replays, from scratch, the statements and values the driver
  bound and committed, in `(era, stamp)` order. Each transaction runs in a
  savepoint, with keys deferred when it has several members. Replicas
  supply only the order, through the qualification-only
  `EncryptedReplica::replayed_positions`.
- **Fix: restoring rows left keys deferred** (`rocky-secure`).
  `restore_images` turned on `defer_foreign_keys` and never turned it
  off. The pragma lasts until the transaction ends, so members replayed
  after a restore in the same transaction ran with every key deferred.
  This affects full rebuilds, rederive and checkpoint pages, and a
  single-member transaction's orphan then applied. Found as seed 2: a
  board that loses its code after a seal and the card on it.
  - Fix: deferral ends after the restore.
  - Tests:
    `tests::compaction::cascades::a_tag_of_an_item_that_drops_after_a_seal_drops_in_every_rebuild`
    and `relational_tests::regressions::a_card_on_a_board_that_lost_its_code_after_a_seal_audits`,
    which both fail without the fix.
- **Fixes from the fuzz** (`rocky-secure` unless named). Each
  regression in `relational_tests::regressions` fails without its fix
  (checked by reverting each part).
  - **Linked scopes compacted to bases at different seals** (seed 20008).
    `compact_replayed` kept a scope's previous base, signed as it was,
    when no member of that scope applied after it. A delete action of a
    linked scope changes its rows without a member of its own: the `cards`
    base stayed at seal 1 (the card) while `boards` moved to seal 2 (no
    board), and a rebuild returned the card without its board. Every
    keyed replayed scope already takes its rows at each finalized seal;
    now the old base stays only while its rows equal the scope's rows
    taken at the target seal, else those rows are signed at the target
    seal. Linked scopes' bases then hold rows at one seal, which a fresh
    replica holding the same scopes would compute.
    `a_cascade_replayed_after_a_seal_survives_the_next_seal_and_compaction`.
  - **Eviction left the rows a rebuild starts from** (seed 20004).
    Eviction kept the scope's rows taken at seals and its replay base, so
    the audit's rebuild restored the evicted board. Eviction deletes
    `secure_replayed_seals`, `secure_replayed_seal_rows`,
    `secure_replayed_base` and `secure_replayed_staged` of the scope.
    `an_eviction_after_a_seal_audits`.
  - **Evicted scopes kept syncing** (the `RestorationGuard` and refused
    `SyncRequest` class). Sync listed every granted scope: an evicted one
    pulled frames its own replica refused ("scope is not held"), and a
    peer answered `Held` for frontiers of scopes it had evicted, then
    refused the catch-up (`NotPermitted`). `changed_scopes` skips scopes
    this replica does not hold, `exchange_frontiers` refuses scopes the
    server does not hold, and a restoration guard is a blocker only once
    its scope is held again. Same regression; it also fails without these.
  - **An evicted scope's positions could not be placed** (seed 20004,
    shrunk again). After an eviction, a frame of the evicted list that
    never arrived blocked finalization. `receive_placements` dropped
    headers of every scope the node has a whole grant for; it now drops
    only scopes it also holds. `pull_placements` first proves the
    partitions the node has a whole grant and key for but does not hold,
    so the peer shows their headers instead of `CustodyRequired`.
    `an_eviction_before_the_list_arrives_finalizes` (fails without either
    part). The proving step and `CustodyRequired` are gone since blinded
    scopes (below).
  - **Evicting a partition whose tuple never arrived** (seed 20033, the
    old seed 1 class). `clear_replayed` refused it as corrupt ("replayed
    scope") after the eviction had started. Such a partition holds no
    rows; the clear forgets only its replay records.
    `evicting_a_list_whose_share_never_arrived_succeeds` (asserts the
    eviction succeeds; the run alone healed before).
  - **Outcome records into an unheld log** (seed 20042, identity-ledger
    class). Evicting `boards` cascades into `cards`; the rederive at its
    end published the pair's outcome record into the `cards` log, which
    the driver had already left, so authoring was refused ("author needs
    a held scope") and `cards` was never evicted. `publish_outcome` signs
    only into logs of scopes this replica holds.
    `evicting_a_list_with_a_board_and_its_card_audits`.
  - **A scope held again kept its seal bounds** (seed 20064, era class).
    After eviction and holding again, the scope looked verified through
    stale bounds, and its returning members replayed in the open era.
    Eviction deletes the scope's `secure_seal_bounds` and
    `secure_seal_drops`, so the scope verifies the last finalized seal as
    a newly held one does. `order_behind_seal` also moves members a seal
    covers that replayed in a later era back into the seal's era.
    `a_list_held_again_after_eviction_keeps_sealed_eras` (fails without
    either part).
    `restoration_never_admits_unknown_own_frames_without_or_above_its_exact_guard`
    now runs one finalization pass before its first snapshot: its fixture
    evicts and holds the scope again, which now verifies at the next pass.
    The refused frames still change nothing.
  - Formats: no wire or checkpoint change; replica format stays
    `rocky.encrypted-replica.v43`.
- **Blinded partition scopes** (seed 20000, `rocky-core`,
  `rocky-credentials`, `rocky-secure`, `rocky-sdk`). c holds only `l1`;
  the owner's seal frontier covers `l0` positions, and a placement page
  answered `CustodyRequired` because a frame header carried the plaintext
  partition tuple. Option (a) alone would not hide it: the scope id
  (`commitment(domain‖table‖tuple)`) was unkeyed, so anyone could confirm
  a guessed tuple. The scope id is now keyed:
  `PartitionTuple::scope` is HMAC-SHA256 (`rocky.partition-scope.v2`)
  under a per-partition naming secret.
  - The header no longer carries the tuple; the sealed body carries it
    (`Opened { partition, payload }`) and a holder refuses a frame whose
    opened tuple differs from the one its registry binds to the header's
    scope ("sealed partition tuple differs from its keyed scope").
  - Placements go to every domain member. A non-holder sees an opaque
    scope id and checks the header only against the author's right to
    write some partition of the table. `CustodyRequired` and the proving
    step are gone.
  - The naming secret is fixed for the partition's life and does not
    rotate with key epochs, so logs and stored scopes are unchanged by a
    rotation. It travels with the group secret (envelope v4 seals
    secret‖name) and in custody offers (to entitled peers only).
    Issuers derive created partitions' names from a domain naming root
    (vault role `partition-naming`, held by the owner and issuers through
    enrollment), so concurrent issuers creating one partition agree.
    Non-issuers name their own-id partitions with their node key.
  - Privacy: a non-holder can no longer learn or confirm a frame's
    partition. Not hidden: positions of one partition share a scope id
    (and an epoch's key hash), so they stay linkable; a revoked holder
    keeps the name and still recognises the scope, but cannot read later
    epochs; any issuer can name any issuer-created partition; creation
    provenance (births) stays in the header.
  - Tests: `a_member_holding_one_list_finalizes_seals_over_another`
    (un-ignored; asserts finalization: it now reaches quiescence and
    shows the base class below),
    `an_unheld_partition_is_placed_without_its_tuple_or_a_confirmable_scope`
    (replaces the custody test),
    `a_frame_whose_keyed_scope_and_sealed_tuple_disagree_is_refused`,
    `keyed_commitments_are_standard_hmac_sha256`.
  - Formats: row format 16, `rocky.encrypted-replica.v47`, group secret
    envelope v4, identity file version 22, enrollment and self-enrollment
    bundle format 3, `secure_partitions(scope, tuple, name)`. Checkpoint
    format and vectors unchanged.
- **Fixes from the soak after blinded scopes** (`rocky-secure` unless
  named). Each regression in `relational_tests::regressions` fails
  without its fix (checked by reverting each part on this base).
  - **A listed checkpoint replaced history placements would finalize**
    (seed 20013, most of the `None` class). After catching up, sync
    imports a checkpoint the graph lists at the next seal when the scope
    "misses" positions through its frontier. Positions of the author in
    other scopes arrive only as placements, pulled after every scope
    synced, so a member holding every frame imported the base, and the
    clear dropped its records of other authors' transactions.
    `replace_with_listed` pulls placements first and decides again.
    `a_listed_checkpoint_waits_for_placements`; seed 20000's regression
    also needs it.
  - **The same for seal rebootstraps** (seed 20033): a newly shared list
    left the domain's scopes unverified and `rebootstrap_scopes_at_seals`
    imported every scope. It also pulls placements before acting on its
    needs. `a_seal_rebootstrap_waits_for_placements`.
  - **Era of a member covered by an earlier seal** (seed 20027, the era
    class). A scope held again first verifies at the last finalized
    seal, while a member an earlier seal covers had already replayed into
    the open era. `order_behind_seal` moves the covered members of a
    scope without seal bounds into the era of the earliest finalized seal
    whose frontier covers their stamp.
    `a_list_held_again_after_two_seals_keeps_the_first_seals_era`.
  - **An evicted own outcome stayed provisional** (seed 20207, author
    side, R3). Eviction kept the author's record (`sealed=0`); a base
    later installed at a seal covering it found no frame to decide, so
    `Applied { sealed: false }` stayed for good. `cover_kept` decides
    the scope's provisional kept members that the base's seal frontier
    covers from its sealed outcomes, as `keep_covered` does for held
    frames. Kept outcomes now store the member's author `sequence`.
    `an_evicted_own_outcome_turns_final_with_a_covering_base`.
  - **A base at an earlier seal never verified the next one** (seed
    20135, `cards` audit, the rebootstrap class). A list shared again
    after a compaction imports the base at an earlier seal with no frames
    under it. `seal_starts` let only no-frames bases account for
    positions through their seal, so the scope counted from the start,
    waited on a seal rebootstrap for good and its rows waited to be
    derived again (reads refused). Any verified base at a path seal now
    starts the count, and an unverified scope whose base sits on the path
    needs no rebootstrap for its pruned prefix.
    `a_base_at_an_earlier_seal_verifies_the_next_seal` (fails without
    either part).
  - **Oracle: other authors' outcomes under a base** (`rocky-sdk`
    driver). A replica keeps outcomes only of members it authored
    (above); one that holds a list by a base covering another author's
    transaction (checkpoint-first share, holding again, the revocation
    restore) reads `None`. The check now accepts `None` there only: a
    non-author, holding all of the transaction's lists, whose installed
    bases are at path seals whose frontiers cover its stamp
    (qualification `EncryptedReplica::base_covers`). It counts as
    coverage "outcome under a base"; authors and holders without a
    covering base are checked as before. The placement regressions also
    assert that no replica needed it.
    `another_authors_outcome_may_go_with_a_covering_base`.
  - Seed 20000's regression asserts outcomes too
    (`keeps_outcomes`), not only finalization.
  - Formats: `secure_kept_outcomes.sequence`,
    `rocky.encrypted-replica.v48`. No wire or checkpoint change.
- **Open:** the 10 seeds this soak left failing are traced under "Fixes
  from the last soak seeds" below; all but 20276 are fixed.
  - Readers of a non-author replica lose other authors' outcomes on
    every checkpoint restore, the revocation restore of every scope
    included. That is the kept-outcome design, not checked further.
- Soak (`ROCKY_SIM_SEEDS=300`, release, seeds 20000–20299, 3–4 nodes,
  40 steps; Apple M5 Pro, shared host). Before (main `dd4fc05f`, after
  the driver's era-only position comparison): 112 passed, 188 failed.
  After these fixes (run as three 100-seed chunks via `ROCKY_SIM_FIRST`):
  182 passed, 118 failed.
  - Outcomes: `None` 47 seeds, `Covered` 20 (the base class above).
  - `FinalizationWaiting`: 31 missing positions, 5 with a rebootstrap or
    pending dependencies; 1 `PendingFrames`.
  - Audits: 9 (6 identity ledger, 3 rows or digests).
  - Era disagreements: 5.
  - Gone: `RestorationGuard` and refused `SyncRequest` (baseline 66
    seeds), the eviction "replayed scope" refusal.
- Soak after blinded scopes (same seeds, chunks and host): 217 passed,
  83 failed.
  - Outcomes: `None` 60 seeds (b 37, c 23), `Some(Applied …)` 2: the base
    class. Seeds that stopped short of quiescence before now reach it
    and run the outcome checks, so this class grew; seed 20000 is one.
  - Liveness: 5 seeds; 3 "missing frames of author N" (was 31).
  - Audits: 10 (b 6, c 4). Era disagreements: 6.
- Soak after these fixes (same seeds, chunks and host): 290 passed,
  10 failed.
  - Outcomes: 0 (was 62). Era disagreements: 0 (was 6). The oracle
    exemption ("outcome under a base") applied 66 times over the run.
  - Audits: 5 (was 10). Liveness: 5 (was 5): 3 pending dependencies
    (20083, 20088, 20120), 2 missing positions (20074; 20276, with
    pending dependencies). No seal rebootstrap wait (was 1).
- **Fixes from the last soak seeds** (`rocky-secure`). Each named
  regression in `relational_tests::regressions` fails without its fix
  (checked by reverting each fix alone, three runs each; the 20290 one
  fails two of three, as below).
  - **A revocation imported a checkpoint into an evicted scope** (20080,
    20127, 20159: identity ledger audits). A learned revocation sets the
    bootstrap intent for every granted scope, evicted ones included; the
    import's identity marks reached the rebuild but not the stored
    ledger. `begin_rebootstrap` refuses an evicting or evicted scope, and
    sync offers and advises no checkpoint for one: holding it again
    imports a base. `a_revocation_imports_no_checkpoint_into_an_evicted_list`.
  - **A superseded head of a retained frame left no stub** (20229, also
    20120). `supersede` deleted a covered frame that c's own outcome
    record names as `previous`; the record waited on dependencies for
    good and blocked the rederive after the board's clear cascaded the
    card away. It now stubs each covered frame a retained frame names.
    This was 20229's earlier nondeterminism.
    `a_superseded_head_of_a_retained_frame_leaves_a_stub`.
  - **An outcome record forked its author's log** (20290, 20083, 20088).
    After a checkpoint switch, the node's own frames in a log wait to
    apply again; an outcome record appended there applied first, and the
    rebuild refused the next pending frame as an author log fork. No
    outcome is appended to a log holding the node's own pending frames.
    `an_outcome_waits_for_its_authors_pending_frames` (fails two of three
    runs without the fix: which member reaches the log first varies).
  - **A rewind split a transaction** (20290, second cause). An arrival
    whose transaction's other member was rewound replayed alone, so the
    card dropped for a missing board while the board replayed after it.
    `replay_arrival` takes the rewound members of the arrival's
    transaction out of the redo and replays them with it, in stamp order;
    if one has no grant, all drop for admission as a whole replay would.
    No deterministic regression: the split needs a rewind order that
    depends on random keys; the seed passes 12 of 12 debug and 3 of 3
    release replays with it.
  - **An outcome record named only the replayed members** (20245:
    `secure_applied` and scope digest audit). A checkpoint reapply
    replayed only the card member of a's pair; c's record named that one
    member, so the full rebuild, replaying both, published another. The
    record now also names the transaction's members already replayed
    here, in stamp order. No deterministic regression (fails about one
    run in four without it); the seed passes 8 of 8 replays with it.
  - **Leaving a scope stranded own frames** (20074: "missing frames of
    author 2"). Partial b wrote a board while a was offline, evicted
    `l0` (refused until custody) and the driver's `unsubscribe` went
    through, so b's frame never synced and a's seal waited for good.
    `unsubscribe` on a partial replica that wrote in the scope now needs
    the same custody coverage as eviction; one that never wrote there
    leaves as before, and leaving twice is a no-op.
    `leaving_a_list_waits_for_custody_of_own_frames`.
  - **A head whose author is revoked** (20276, holder side). c read b's
    frame, then b was revoked while c was severed; c's next write names
    b's frame as `previous`, and the revocation rejects every frame of b.
    A rejected non-backlink head whose author is revoked now counts as
    satisfied: the reader's write does without it.
  - Formats: none changed (`rocky.encrypted-replica.v48`).
- **Open: a revoked head at a peer that never saw it** (20276). Closed
  below under "Revoked heads and deterministic simulation".
- **Nondeterminism.** Steps replayed identically, but node keys, nonces
  and lineages were unseeded (20229, 20245, 20290 showed in some runs
  only). Closed below.
- Soak after these fixes (same seeds, chunks and host, three full runs,
  the last on the final code): **299/300** each time; the only failure is 20276 (above). Each chunk of
  100 took about 280 s.

## Revoked heads and deterministic simulation

- **Revoked heads resolve by evidence** (20276, `rocky-secure`). Every
  frame of a revoked author is refused, and lane L let a write that read
  one do without it. A peer that never received the head could not tell
  it from a frame still on its way: revoked frames are never served, so a
  waited for good. Lane L's attempt to serve the revoked frame failed the
  page on its refused chain.
  - Rule: after a catch-up, a replica names the unknown non-backlink
    heads its pending frames in the scope wait on
    (`RowMessage::RevokedHeadsQuery`, at most 64, only once its own
    revocation set is not empty). The peer answers with the signed
    header and credential chain of each it holds, as a frame or as kept
    evidence, whose chain its revocation set ends (`RevokedHeads`); never
    a body. The receiver keeps evidence that verifies (header signature,
    chain digest named by the header, the requested scope, a head its
    pending frames name) in `secure_revoked_heads`. `dependencies_ready`
    counts a missing head with kept evidence as satisfied while the
    receiver's own revocation set ends the chain, the same predicate as
    for a held frame, earlier than the dependent and of its scope. A
    backlink never resolves so.
  - Why not alternatives: evidence carried with the dependent frame
    misses a frame that arrived before the revocation (a pending frame
    counts in the frontier and is never sent again); a signed per-author
    frontier in the revocation would change what a revocation refuses
    (today every frame of the author) and the revoker cannot know which
    heads others read. The decision is the revocation's: no timeout.
  - Every holder decides alike: a holder of the head (applied or
    refused) and a holder of evidence both satisfy it once they know the
    revocation; before that both wait. The relational oracle checks the
    write's outcome on every holder.
  - Privacy: the query is a row message gated like `Range` (proved
    holding, `authorize_peer`, the server holds the scope); answers are
    headers of that scope only, which a holder would have received
    anyway; no partition tuple travels (headers name blinded scopes).
  - Tests: `relational_tests::regressions::a_head_whose_author_is_revoked_resolves_at_a_peer_that_never_saw_it`
    (the 20276 repro; fails without the rule: "missing frames of author
    1026", pending `Dependencies`) and
    `tests::revocations::a_revoked_head_resolves_by_evidence_from_a_peer_that_holds_it`
    (unrevoked chains, evidence for unnamed heads and receivers without
    the revocation change nothing).
  - Formats: `rocky.encrypted-replica.v49` (`secure_revoked_heads`),
    link handshake `rocky.link.v20` (row messages). Frames, checkpoints
    and vectors unchanged.
- **Deterministic simulation** (`rocky-platform` feature `simulation`,
  enabled only by `rocky-sdk`'s dev-dependency). `driver::run_steps`
  installs `entropy::simulation::seed(seed)` on its thread. There, and
  only there:
  - `rocky_platform::entropy::{fill, random}` draw from the seed (a
    SplitMix64 stream). Node keys, identity and tip keys, AEAD and
    enrollment nonces, envelope ephemerals, challenges, lineages,
    transaction handles and retry seeds now call it; every other thread
    and every build without the feature use `OsRng`.
  - `rocky_platform::time::since_epoch` is 2026-01-01 plus the paused
    task clock's elapsed time plus 1 ms the driver adds per step (the
    replica's stamps, credential times, activity times).
  - `spawn_blocking` runs in place, `dedicated` tasks are local tasks
    and `Held::wait_released` returns at once, as in a browser Worker.
  - The memory hub sends discovery hints in name order (`BTreeMap`).
  - Each relational seed runs on its own current-thread runtime
    (`driver::run_isolated`); built with `--cfg tokio_unstable` its
    `RngSeed` follows the seed too.
  - Production changes: the runtime's session and placement nonces were
    clock-derived and are now 32 random bytes (unique under a paused
    clock); no API takes caller randomness.
  - Test: `relational_tests::one_seed_replays_identically` (seeds 3 and
    20245 twice: same steps, outcomes, coverage and per-member digests
    of frontier, rows and replayed positions). The soak prints
    `seed N replay <digest>` per seed.
  - Not fixed by the seam: tokio picks among ready `select!` branches
    with a per-runtime random seed that stable tokio cannot set. On a
    stable build 1 in 6 replays of 20003 differed in one healing-time
    stamp; with `RUSTFLAGS="--cfg tokio_unstable"` 16 of 16 replays were
    identical. 19 of 20 sampled soak seeds replayed identically on a
    stable build.
- Release soak (seeds 20000..20299, 100-seed chunks, this Mac, stable
  tokio), run twice: 300/300 and 300/300 (chunks 203/197/191 s and
  205/200/191 s). The runs agree on every outcome; 295 of 300 replay
  digests are byte-identical. 20048, 20090, 20154, 20279 and 20295
  passed both times with different digests, from the `select!` seed
  above.
- **Closed: a reference advances the hybrid clock** (2026-11-04). With a
  wall clock that stands still, seed 20135's regression failed its
  audit: a's card pinned to b's board got the board's clock, as an
  intent's stamp passed only the heads of the scope it writes. A full
  rebuild then replayed the card before its board and dropped it while
  the projection kept it. Lagging clocks, or coarse ones (a browser's
  millisecond clock), gave the same tie or inversion.
  - Fix (`author_log_frame`, shared by native and wasm): an intent's
    clock also passes `MAX(clock)` of `secure_replayed`, every member
    replayed here in any scope. A row it references or reads is replayed
    here (its member's record) or restored from a base of an earlier
    era, so canonical `(era, stamp)` order puts it first. Indexed by
    `secure_replayed_clock`. Authoring one single-row intent takes one
    statement more: 80 / 44, the budget now ≤ 44.
  - Seam: `entropy::simulation::freeze` keeps the simulated wall clock at
    its epoch; a scenario chooses it with `Wall::Frozen`.
  - Tests: `relational_tests::regressions::a_card_on_a_remote_board_replays_after_it_under_a_frozen_clock`
    (20135 frozen; failed before at step 2: "projection differs from
    full rebuild: cards") and
    `tests::replayed_constraints::a_child_replays_after_a_parent_stamped_ahead_of_its_wall_clock`
    (a team stamped by a clock 60 s ahead; the member's stamp passes it,
    and every replica and a rebuild keep the member; fails without the
    fix).
  - Formats: `rocky.encrypted-replica.v50` (the index). Frames, wire and
    checkpoints unchanged.
  - Release soak (seeds 20000..20299, moving clock, this Mac): 300/300
    (chunks 368/362/351 s).
- **Closed: received stamps are bounded** (2026-11-04). Receipt checked
  only that a frame's stamp passed its dependencies'. A stamp at or past
  2^63 was stored negative, so SQL replayed it first while Rust ordered
  it last and `MAX(clock)` skipped it; a stamp near `u64::MAX` exhausted
  the hybrid clock of every replica that observed it; and a peer whose
  clock ran ahead dragged every replica's clock and won every concurrent
  last-writer-wins conflict for that span.
  - Validity (deterministic, permanent): a clock is at most
    `rocky_core::MAX_CLOCK` (`i64::MAX`). `check_header` refuses a frame
    above it; `SecureStamp`, `LogPosition`, a checkpoint statement's
    clock and a checkpoint page row's clock refuse it when decoded.
    Authoring (`next_clock`, rows and intents alike) returns an error
    instead of passing it. Clocks bind to SQL through `clock_value` /
    `stored_clock` (checked) instead of `as` casts.
  - Hold (eventual, never a refusal): a received frame stamped more than
    `MAX_CLOCK_AHEAD` (5 min) ahead of this replica's wall clock waits as
    a pending log head (`WaitReason::ClockAhead`) after its other checks
    pass, single frames, bundles (the latest member) and held-tip frames
    alike; its log and every frame stamped after it wait behind it. It
    applies once this clock is within 5 min of it, so every honest replica
    applies the same frames and a fast clock costs delay, never data. A
    replica's own frames never wait. The waiting frames use the existing
    pending storage and its bounds; only their log heads are kept in
    memory. Read completeness (`pending_now`) ignores them: they are as
    if not arrived yet, and everything behind them is stamped later.
  - Wake: `EncryptedReplica::clock_wait` gives the earliest due time; the
    SDK's local recovery driver sleeps until it on `rocky_platform`'s
    timer (native and wasm), so a waiting frame applies on time with no
    other frame or sync.
  - Visibility: `BlockerKind::ClockAhead { node, count, seconds }`,
    `ClearsBy::Clock`, per scope and author, in the existing blockers of
    node and database status.
  - Bases are exempt: a checkpoint's clocks were applied by its signer
    under the same hold, so they are within 5 min of that trusted signer's
    clock; the validity bound still applies to them.
  - Tests: `tests::replayed_constraints::clock::*` (a stamp past the bound
    is refused and stored nowhere, one at the bound waits, stamps and log
    positions past it fail to decode, `next_clock` refuses past it; a
    peer an hour ahead waits with its successor, shown as a clock blocker,
    the receiver's own write stays behind it, nothing applies 1 min before
    due, both apply after it and every replica and a rebuild match the
    canonical replay; 4.5 min ahead applies at once) and
    `simulation::clock::frames_from_a_clock_ahead_apply_when_this_clock_is_near_them`
    (links severed after receipt; the frames apply after the due time by
    the recovery timer alone, and not before). Each fails without the
    hold or without the timer.
  - Formats: `rocky.encrypted-replica.v51` and link handshake
    `rocky.link.v21` (the stricter validity rule). Frame, checkpoint and
    page encodings unchanged.
  - Checks: `./scripts/lint` 0; `./scripts/check --unit --integration` 0
    (256 unit, 1405 integration passed); `./scripts/check --wasm` 0
    (831 passed, 1 ignored); release `relational_soak` 300 seeds from
    20000 in three chunks of 100, all passed (410 s, 420 s, 356 s).
- **Closed: a fast clock is reported on both devices** (2026-11-04).
  The hold above showed the receiver a `ClockAhead` blocker, but the
  device whose clock ran ahead never learned that its writes waited
  elsewhere, and no transition was logged.
  - Handshake: each hello carries the sender's wall clock (microseconds,
    signed with the hello). Each side records the peer's clock less its
    own at receipt as `Link::clock_offset`; transit and handshake waits
    are seconds at most, negligible against `MAX_CLOCK_AHEAD`.
  - Status: `DatabaseStatus.alerts: Vec<Alert>` (`kind`, `since`) reports
    conditions that delay or degrade but block no scope; blockers stay
    for scopes. `AlertKind::ClockAheadOfPeer { node, seconds }` (this
    device's changes wait on `node`) and `PeerClockAhead { node, seconds }`
    (`node`'s changes wait here) hold per peer, replaced by every
    handshake and cleared by one within the bound; in memory, at most 64
    peers, only node id and seconds. Alerts are part of the status token,
    so `status_after` wakes when one appears, changes or clears.
  - Logs: `tracing::warn!` when an alert appears or clears, and when a
    peer's frames start waiting for this clock (`info!` when none wait
    any more); never per status poll.
  - Tests: `tests::admission::each_side_of_a_handshake_learns_the_others_clock_offset`
    (an hour ahead and none, both ends) and
    `simulation::clock::a_link_tells_both_devices_when_one_clock_runs_ahead`
    (a node's own tasks see its clock an hour ahead: after a new link the
    fast node shows `ClockAheadOfPeer` and the other `PeerClockAhead`,
    each delivered to a `status_after` wait pending before the link; the
    fast node's write then waits behind a `ClockAhead` blocker; both
    alerts clear after its clock is right and a new link, again through
    `status_after`; 4 min ahead raises none; the held write applies once
    the receiver's clock reaches it). Both fail with the handshake offset
    forced to 0.
  - Formats: link handshake `rocky.link.v22`; replica and frame formats
    unchanged.
  - Open: a coordination service's `ClockSkew` refusal still surfaces
    only as the failed call's `clock_skew` code with `coordinator_time`,
    not as an alert: it names no peer and has no clearing event.
  - Checks: `./scripts/lint` 0; `./scripts/check --unit --integration` 0
    (256 unit, 1408 integration passed); `./scripts/check --wasm` 0
    (832 passed, 1 ignored); release `relational_soak` 300 seeds from
    20000 in three chunks of 100, all passed (388 s, 387 s, 557 s).
