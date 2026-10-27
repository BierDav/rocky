# 4. Cheaper commits

Status: planned. Owner: `rocky-sdk` (`Database::transaction` default),
`rocky-secure` (authoring in `replica/author.rs` and `replica/bundle.rs`,
receive and apply). Depends on [phase 1](01-see-the-wait.md). Effort: one
to two weeks. Follow the [execution contract](README.md#agent-execution-contract).

## Objective

Make the work a write holds the lock for smaller, and stop the Rust SDK
from paying a device flush on every interactive commit. This also closes
the single-row receive statement gate that phase 9 of features_11 left
open.

## Inputs

- Phase 1 evidence: the single-row author statement count and trace, and
  the probe in both durability modes.
- The features_11 phase 9 evidence. The 83 statements of a single-row
  receive trace to operation admission and storage, pending and
  quarantine passes, identity and seal checks, revision signing,
  transactions and post-commit recovery. Subscriptions, role
  configuration and the newest checkpoint are already cached and read at
  most once per frame.

## Settled

- **Buffered Rust default.** `Database::transaction` opens its connection
  with `Durability::Buffered`. Its doc comment states the guarantee: the
  write is visible to observers and readers on return, survives an
  application crash, and becomes durable and publishable at the
  background flush. Callers that need flushed commits open a connection
  with `Durability::Flushed`. Tests that relied on the old default switch
  to that explicitly; they don't wait on timing. Kotlin is unchanged; it
  is already buffered.
- **Per-group, not per-frame.** Work that depends only on the transaction
  or the group runs once per group, not once per frame. Examples: revision
  signing, pending and quarantine passes, post-commit recovery triggers,
  and blocker evaluation for an unchanged scope. Phase 9 showed this
  pattern for the admission head.
- **Caches follow the phase 9 rule.** A cached read is invalidated inside
  the transaction by the table-specific write counters, and cleared on
  rollback and on context restoration. Each new cache gets a test that a
  write in the same transaction is seen, and that rollback clears it.
- **Authoring reads each row once.** `author_frame_carried` currently
  reads each written row twice: once for the `before` check and once for
  stamps, creation and witness. It must read once and reuse the record.
  The author tip, scope id, identity allocation high-water mark and
  finalized flag are read once per bundle, not once per frame, when a
  bundle spans several frames of one scope.
- **Admission is never skipped.** A cut may avoid repeating a check
  already made in the same transaction on the same inputs. It may never
  remove a check, move one after the commit, or trust a cache across an
  authority, key or grant change.
- **Format.** Prefer no persisted shape change. If an index is needed, it
  lands within phase 5's single bump from 39 to 40, coordinated through
  that phase's change. Otherwise defer it and record why.

## Ordered slices

1. Buffered `Database::transaction` default, doc comment, and adjusted
   tests. Rerun the L0 probe. Land this slice first; it needs nothing
   else and is the largest desktop L0 win (Flushed p50 77.0 ms, Buffered
   3.5 ms in phase 1).
2. Authoring cuts: single row read, per-bundle reads, per-group work.
   Statement-count regression test for a single-row local author, which
   fails if the count rises above the new value.
3. Receive cuts: per-group passes and checks. Keep the existing
   `an_ordinary_update_applies_in_few_statements_per_row` assertions and
   tighten the single-row bound to the new count.
4. Run the full-rebuild, finalization, audit and LtHash equality tests
   (`lthash_100000_matches_phase_start` must reproduce the pinned digest).
5. Rerun L0 and L2 on desktop, and on phone where available, plus the
   desktop sync non-regression rows.

## Completion gate

- Single-row receive is at most 45 statements, and single-row local
  author is at most half its phase 1 count, both asserted in tests.
  32-row receive stays at or below 6.5 statements per row.
- Desktop L0 interactive write p95 ≤ 3 ms, and observer notification p95
  ≤ 5 ms after `commit()` returns. Phone L0 p95 is recorded against
  ≤ 10 ms.
- Equality and digest tests pass unchanged. The desktop sync
  non-regression rows hold.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

Changing what authoring decides: conflict checks, clocks, stamps,
witnesses or proofs. Also crypto changes and lock behavior (phases 2
and 3).
