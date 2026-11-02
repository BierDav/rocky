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
