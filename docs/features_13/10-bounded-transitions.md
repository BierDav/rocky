# 10. Bounded transitions

Status: planned (2026-10-31) against `main` after phase 6 (`dd9a8a35`).
Slices 1–2 (inventory and design) touch no runtime and may run once
phase 8 lands. Implementation (slice 3 on) starts only on the trigger
below. Owner: `rocky-secure` (`replica/replay.rs`, `compaction.rs`,
`schemas.rs`, `migrate.rs`, `tips.rs`, `checkpoint/`, `lock.rs`). Depends
on [phase 8](08-intent-only.md) for the code it changes and on
[phase 9](09-qualification.md) for the trigger. Effort: one to
one and a half weeks for slices 1–2, then about a week per transition
kind. Carried over from features_12 phase 7. Follow the
[execution contract](README.md#agent-execution-contract).

## Why it is separate

A transition replaces a partition's rows, projections and history in one
SQLite transaction, so readers never see a mixture and a crash rolls the
whole replacement back. Its work grows with the partition or with the
open tip. features_12 phase 1 recorded holds of 2.4 s (desktop checkpoint
installation, 100k rows), 39.5 s (Pixel installation) and 89.1 s (Pixel
L3 compaction). Phase 2's gate only delays the start of a transition
while interactive work waits. Once a transition starts, it blocks writes
until it finishes.

Readers already use their own WAL snapshots, so a long transition hides
its intermediate states from them. The cost is the writer: an
interactive write waits for the whole transition. A bounded transition
therefore has to do its preparation outside the replica lock, or in
bounded steps under it, and publish in one bounded transaction.

## Transitions after phase 8

Read from code at `dd9a8a35`. Phase 8 deletes the register restore and
the constraint resolver inside several of these, so slice 1 re-reads
every row. Paths are under `crates/rocky-secure/src/replica/`.

| Kind | Entry | One hold covers | Grows with |
| --- | --- | --- | --- |
| R. Receive rewind | `replay.rs` `replay_arrival` → `rewind`, `redo`, inside the group transaction (`group.rs` `run_group`) | undo of the closure, the arrival, redo of every transaction in its own savepoint, republished outcome records | unsealed transactions in the transitive closure over members and `linked_scopes` (FKs and assertions across partitions) |
| F. Seal finalization | `compaction.rs` `finalize_ready_inner`, one guard over up to `limit` seals, one transaction per seal → `finalize_seal_impl`, `order_behind_seal`, `seal_replayed` | late members after each cut, rewound and redone in the new era; covered members marked sealed | late transactions after each cut and covered members, times seals per call |
| S. Live schema switch | `schemas.rs` `switch_schema_now`: staging transaction, then `finish_plan` under one guard; `migrate.rs` `switch`, `switch_constrained`; `replay.rs` `rebuild_replayed`, `reshape_undo` | whole-table copy, index builds, unsealed undo rewrite, `verify_shadow` over every row, and `switch_tip` inside the switch transaction | table rows and unsealed undo, plus a tip rebuild |
| T. Tip workspace rebuild | `tips.rs` `rebuild_tip_workspace`, reached from a non-forward `switch_tip`, tip admission (`admit_tips_with_stage`) and `withdraw_schema_switch` | every scope of the domain | domain |
| M. Member migration replay | [phase 7](07-intent-migrations.md): the program per held partition, one quiet-gate step each | one partition's program | partition |
| I. State installation | `checkpoint/transfer.rs` `switch_to_staged`, `checkpoint/pulled.rs` `install_pulled`, `compaction/rebootstrap.rs` `install_needed_pulled` (every scope of a domain) and `replay_at_seals` | logs, stubs, pages, identity ledger, reapply of retained operations | scope history; no replayed rows today (gap 3) |
| P. Projection rebuild | `rebuild.rs` `rebuild_projection` | `restore_rows` plus `reproject` | scope (gap 2) |
| C. Compaction | `compaction.rs` `compact_to`: one guard, one transaction per scope | overlay replay since the base | operations since the base; replayed scopes never prune (gap 3) |

These are exempt because they only run before `Database::open` returns:
`finish_plan` and `load_typed`'s `run_plan` at open, before the replica
lock exists, and `checkpoint/audit.rs` `verify_open`.

## Gaps found while planning

These are not phase 10 work. Each has an owner (decided 2026-10-31).

1. **The rewind bound is not enforced.** `ROUTINE_SEAL_TRANSACTIONS`
   (128, `replay.rs`) says that "a rewind undoes and redoes at most this
   many". In fact it only triggers an owner's routine seal
   (`rocky-sdk` `node/recovery.rs` `seal_routinely`). A rewind follows
   the transitive closure and has no cap. It stays unbounded while no
   owner is online, and while a staged schema switch blocks finalization
   (`finalize_ready_inner`). The comment is corrected with this plan.
   Phase 9 slice 6 measures the cost; long rewinds are this phase's
   kind R.
2. **`reproject` on replayed scopes.** `typed.rs` `reproject` deletes the
   whole typed table and refills it from row records, but replayed scopes
   keep none. Its callers include `rebuild_rows` (used by
   `rebuild_projection`, `rebuild_tip_workspace` and
   `withdraw_schema_switch`) and `switch_to_staged` when it rewrites.
   No guard against a replayed scope was found on these paths. Owner:
   [phase 8](08-intent-only.md), with tests.
3. **Replayed checkpoints do not exist.** `checkpoint/prune.rs` refuses
   replayed scopes (`NoCheckpoint`). After phase 8, no frame is ever
   pruned and every bootstrap replays the full history, which threatens
   the member-file and phone-bootstrap sync rows. Owner: phase 8, which
   adds replayed checkpoints installed into `waiting` partitions.
   Installing one into a live partition is kind I here.
4. **`compact_to` updates `secure_finalized` in autocommit statements**
   after its per-scope transactions. A crash between those statements
   leaves `compacted=1` with the older depths still present. Owner:
   phase 8 moves both into the last transaction.

## Trigger

Implementation starts only if one of these holds:

- Phase 9 shows a transition or rewind overlapping interactive writes,
  with a write above the README's max targets. This counts in the
  shopping app scenario, in a recorded real workload, or in the
  relational workloads of [features_14 R6](../features_14/README.md#r6-qualification-for-relational-applications).
- The product decides that bootstrap, rebootstrap, compaction or rewinds
  must never stall local writes.

## Settled

- **Design first.** Slice 2 is a reviewed design, not code. It must
  establish:
  - a stable source cut;
  - invisible staging;
  - how writes during preparation are handled, with a liveness argument;
  - bounded verification and index work;
  - a bounded publication transaction;
  - recovery states;
  - bounded cleanup.

  For rewinds it must also show that a reader never sees a partially
  redone suffix. These are the requirements of the features_12 phase 2
  evidence ("What a solution must establish"). Read it with
  `jj file show -r 1dc6522c docs/features_12/02-bounded-holds-evidence.md`.
  The design below is settled. Slice 2 turns it into schemas and state
  machines. A point that proves unworkable stops the slice with its
  evidence, under the execution contract; it is not quietly replaced.
- **One format bump.** It adds one durable journal, `secure_transition`,
  plus the staging the design needs. There is one new replica version
  (`generation.rs` `FILE_IDENTITY`) and no reader for the earlier one.
  Don't overload existing cursors or flags (`secure_rebootstrap.cursor`,
  `secure_projection_plan`).
- **Same guarantees.** These keep their meaning:
  - checkpoint commitments;
  - identity and provenance checks;
  - handle retries;
  - revision and flush ordering;
  - seal-relative drop maps;
  - outcome records;
  - audit results.

  Observers and the outbox see a transition only after its publication
  commits.
- **In place below a bound.** Each kind computes its work before it
  starts: transactions in a rewind closure, rows and undo for a switch,
  history for an installation. At or below the kind's named bound, the
  transition runs in place as one quiet-gate step, as it does today, and
  writes no journal. Only above the bound does it stage. Size each bound
  so that unit cost times the bound is at most 12.5 ms on the phone,
  which is half the phone p99 hold target. Bounds are named constants
  next to the code they bound. Short online rewinds, the common case,
  stay on today's path.
- **Hold targets.** The targets are features_12 phase 2's:
  - desktop: p99 ≤ 10 ms and max ≤ 50 ms per acquisition;
  - phone: p99 ≤ 25 ms and max ≤ 100 ms per acquisition.

  They apply to every preparation step, every publication and every
  cleanup step.

## Design

- **Staged replay.** After phase 8, every kind except C replays
  transactions over a partition group from some starting point:
  - R and F rewind to the earliest affected order and redo;
  - T rewinds to the fork and replays the new path;
  - M replays the program;
  - I and P start from a base and replay.

  Above its bound, a kind stages that replay in a scratch SQLite database
  instead of in place. The scratch database is a sibling file of the
  replica with the same installed schema under the real table names, so
  statements run with SQLite's own constraint semantics and the same
  engine identity. Replay, admission and undo capture get the connection
  as a parameter; they don't choose it themselves. Attaching the scratch
  database is ruled out: in WAL mode, a commit across attached databases
  is not atomic. The scratch file is `<replica>.transition`, in WAL mode
  with `synchronous=NORMAL`, created with the replica file's permissions
  and file protection class; an in-memory replica uses an in-memory
  scratch connection. One stage exists at a time per replica; a second
  transition above its bound waits for the first to publish.
- **Source cut.** Preparation copies the partition group's rows and its
  open `secure_replayed` rows, including undo, from one reader-connection
  snapshot, outside the replica lock. The journal records the snapshot's
  revision and the last order applied for each partition. In scratch,
  undo restores the starting point.
- **Writes during preparation.** These are transactions, not row
  changes:
  - A local commit or in-order arrival applies live as usual, and the
    stage queues it, because it orders after the queued work.
  - An arrival that would rewind into the staged range is not applied
    live. It joins the stage and remains provisionally received until
    publication.
  - An arrival ordered before the stage's start rewinds inside the
    scratch database.

  Scratch steps of `STAGE_TRANSACTIONS` run without the replica lock.
  Under the lock, publication checks that the scratch database has
  replayed every transaction the partition group holds live.

  Liveness: under the starvation guard, each step replays at least
  `STAGE_TRANSACTIONS` transactions while at most 8 local commits arrive.
  `STAGE_TRANSACTIONS` > 8 therefore converges. After `SCAN_PASSES`
  refused publications, the kind falls back to one quiet-gate step,
  counted as `transition_fallback`, like `scan_fallback`.
- **Publication.** Publication runs in one transaction under the lock.
  It writes the staged rows into live, for the keys the scratch database
  or live changed since the cut (taken from the session captures that
  already produce undo). It replaces the group's open `secure_replayed`
  rows, writes outcome records and the outbox, and sets the journal to
  `published`. Its work is bounded by `PUBLISH_ROWS`.
- **Whole tables.** When a transition covers every partition of a table,
  as in S, M and a domain-wide T, it publishes by renaming instead. A
  shadow table in the main database gets its final indexes and
  constraints while empty. It is then filled in bounded steps, so index
  work is paid per step. Publication renames it in, using
  `legacy_alter_table` as `rebuild_replayed` does, which is O(schema).
  Verification accumulates an LtHash per step and never re-reads the
  table at publication (today's `verify_shadow` does). A switch's
  `switch_tip` becomes its own transition instead of running inside the
  switch transaction.
- **Recovery.** At open, the journal is read:
  - `preparing`: discard the scratch database (it is disposable) and
    restart preparation;
  - `published`: resume cleanup.

  Publication is a single transaction, so a mixed state is never
  durable.
- **Cleanup.** Retired tables and staged rows are deleted in steps of
  `CLEANUP_ROWS`. A table is dropped only once it is empty, because
  `DROP TABLE` frees every page in one statement.
- **Overflow.** A partial-partition publication above `PUBLISH_ROWS`
  (for example rebootstrapping a long-diverged live partition) marks the
  partition group `waiting` in its first publication step. That reuses
  the incomplete-partition refusal for reads and writes. It then applies
  in steps of `PUBLISH_ROWS` and releases the group in the step that
  sets the journal to `published`. The replica lock stays bounded; the
  group is briefly unavailable, and the SDK reports that as it reports an
  incomplete partition. A refused interactive write is not a latency
  sample. Qualification counts these windows and their length.
- **Bounds.** `STAGE_TRANSACTIONS`, `PUBLISH_ROWS`, `CLEANUP_ROWS` and
  each kind's in-place bound follow the 12.5 ms phone rule above, from
  slice 1's unit costs. `STAGE_TRANSACTIONS` is at least 9.

## Ordered slices

1. **Inventory on phase 8's code.** Re-read the table above:
   - remove what phase 8 deleted;
   - confirm gaps 2 and 4;
   - add any kind phase 7 or 8 introduced.

   Measure, in release, desktop and Pixel where available:
   - unit cost per kind (rewound transaction, copied row, built index
     row, published row);
   - the worst hold per kind on the 100k fixture and on a 4× routine-seal
     open tip.

   Derive the in-place bounds. Record the results in
   `10-bounded-transitions-evidence.md`.
2. **Design.** Turn the design above into a reviewed document. Write
   down:
   - the journal and staging schema;
   - the state machine for each kind;
   - the liveness argument;
   - the format bump.

   A point that proves unworkable stops here and goes to the owner with
   its evidence.
3. **Mechanism.** Add:
   - the journal;
   - the scratch database lifecycle;
   - connection-parametric replay, admission and undo capture;
   - publication, cleanup and open-time recovery;
   - `transition_fallback`;
   - work counters per acquisition for preparation, publication and
     cleanup.

   No kind uses it yet. Tests: a scratch replay equals in-place replay
   on the phase 5 convergence fixtures (rows, drop map, outcome records).
4. **Kinds, worst first.** Implement one kind per change, ordered by
   phase 9's worst overlap; without data, use the order R, F, S, T, M,
   I, P, C. Each kind keeps its in-place path below its bound.
5. **Interrupt and interleave.** For each kind:
   - Arm a `CrashPoint` (`crash.rs`) after every preparation step, at
     publication and after every cleanup step. Compare the reopened and
     resumed result with an uninterrupted run: rows, projections,
     operation statuses, identities, logs, base summaries, drop maps,
     outcome records and audit results.
   - Interleave local writes, incoming frames (earlier and later
     ordered), grants, revocations and tip changes at deterministic step
     boundaries. Use pause points, not sleeps.
   - Assert the work per acquisition against the named bounds.
   - Add a staged rewind to the simulation seeds; every seed converges.
6. **Qualification.** Rerun:
   - the L1–L4 probe, with a staged transition overlapping the probe;
   - the phase 9 shopping scenario and its no-owner 4× open tip;
   - the 100k migration;
   - the sync non-regression rows.

   Pair each run with the phase-start commit.

## Completion gate

- Every transition kind present after phase 8 meets the hold targets
  above in L1–L4 on desktop, and on the phone where measured. A miss is
  listed with its kind, step and profile.
- No interactive write waits for more than one bounded step. This is
  asserted with `interactive_steps`. `transition_fallback` is zero in
  the qualification runs, or each occurrence is reported.
- Without changes, these tests pass:
  - the full-rebuild, finalization, audit and LtHash equality tests;
  - the phase 4 and 5 convergence fixtures;
  - the successor-tip tests.
- The phase 4 statement budgets and the sync non-regression rows hold.
- `./scripts/check` passes. The Kotlin and shopping app checks pass if
  the SDK changed.

## Not in scope

- Adding replayed checkpoints (gap 3).
- Preempting a running step.
- Changing seal authority or the routine seal constant.
- Speeding up a single in-place step beyond what its bound needs.
