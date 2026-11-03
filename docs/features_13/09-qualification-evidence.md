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
- **Open** (minimized, kept as `#[ignore]` reproductions in
  `relational_tests::regressions`):
  - Seed 20008: linked scopes rebuild from different seals. A board
    delete cascades its card, replays after seal 1 and is covered by
    seal 2. Seal 2 covers no `cards` member, so it takes no `cards` rows.
    After compaction, the full rebuild restores `cards` at seal 1 and
    `boards` at seal 2, and the card returns without its board. Taking
    linked scopes' rows at each seal would also need open cross-scope
    undo, so this is not a local fix.
  - Seed 20004: a rebuild after an eviction replays the evicted scope.
    A board, a seal, then a partial member evicts the list, and its
    audit's rebuild shows the evicted board.
  - Seed 20000: a member holding only `l1` never finalizes a seal over
    the owner's `l0` frames ("missing frames of author 1 through the
    seal's frontier").
  - Seed 20007 (shrinks poorly): after a cascade delete in `l0`, the
    full rebuild's identity ledger differs from the projection.
  - Seed 1: evicting a list the member was never shared. The release is
    accepted, the eviction fails with "replica consistency check failed:
    replayed scope", and a `RestorationGuard` blocks the member from then
    on. The driver now evicts only shared lists.
- Soak (`ROCKY_SIM_SEEDS=300`, release, seeds 20000–20299, 3–4 nodes,
  40 steps, before the driver's era-only position comparison): 146
  passed, 154 failed. Every failure class matches an open item above or
  below; none is a reference mismatch of rows.
  - Audits:
    - after eviction: 26 seeds;
    - linked-scope rebuild (`cards`, `boards`): 7;
    - identity ledger: 8.
  - Outcomes: 11 seeds where a holder reports no outcome for a
    transaction that applied, though it holds its rows (it received them
    through a base, not replay).
  - Liveness, mostly after eviction:
    - `RestorationGuard`: 27 seeds, plus 32 with a refused `SyncRequest`;
    - `FinalizationWaiting` alone: 21 seeds (seed 20000's class).
  - Reached: 566 applied and 19 dropped transactions checked against the
    reference; 162 evictions; crash points Apply, Compact, Import, Redo,
    Rewind, Settle, Switch and UndoDeletion fired; 205 rebootstrap and 37
    import waits observed.

