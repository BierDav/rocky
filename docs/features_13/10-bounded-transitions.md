# 10. Bounded transitions

Status: deferred; starts only on the trigger below. Owner:
`rocky-secure`: checkpoint installation, rebootstrap, live schema switch,
projection rebuild, compaction, finalization, tip workspace rebuilding,
and from this roadmap rewinds and member migration replay. Depends on
[phase 9](09-qualification.md). Effort: not estimated until the design
slice lands. Carried over from features_12 phase 7. Follow the
[execution contract](README.md#agent-execution-contract).

## Why it is separate

A transition replaces a scope's rows, projections and history in one
SQLite transaction, so readers never see a mixture and a crash rolls the
whole replacement back. Its work grows with the scope. features_12
phase 1 recorded holds of 2.4 s (desktop checkpoint installation, 100k
rows), 39.5 s (Pixel installation) and 89.1 s (Pixel L3 compaction).
A rewind across a long open tip and a member's per-partition migration
replay have the same shape. Phase 2's gate keeps all of them from
starting while interactive work waits; once started they block writes.

## Trigger

Start this phase only if one of these holds:

- Phase 9 shows a transition or rewind overlapping interactive writes in
  the shopping app scenario, or in a recorded real workload, with a write
  above the README's max targets.
- The product decides that bootstrap, rebootstrap, compaction or rewinds
  must never stall local writes.

## Settled

- **Design first.** Slice 1 is a reviewed design, not code. It must
  establish what the
  [features_12 phase 2 evidence](../features_12/02-bounded-holds-evidence.md#what-a-solution-must-establish)
  lists: a stable source cut, invisible staging, handling of writes
  during preparation (with a liveness argument), bounded verification
  and index work, a bounded publication transaction, recovery states and
  bounded cleanup. For rewinds it must also show that a reader never sees
  a partially redone suffix.
- **Format change allowed.** A generation selector and a durable install
  journal are the expected candidates. One new replica version, with no
  reader for the earlier one. Don't overload existing cursors or flags.
- **Same guarantees.** Checkpoint commitments, identity and provenance
  checks, handle retries, revision and flush ordering, seal-relative drop
  maps and audit results keep their meaning.

## Ordered slices

1. Design, recorded in evidence, with the format decision.
2. Implementation per the design, one transition kind at a time,
   starting with whichever phase 9 measured as the worst overlap.
3. Tests: interrupt after every step and compare the resumed rows,
   projections, operation statuses, identities, logs, base summaries,
   drop maps and audit results with an uninterrupted run. Interleave
   local writes, incoming frames, grants and revocations, and tip changes
   at deterministic boundaries. Count all work per acquisition.
4. Rerun the L1–L4 probe and the sync non-regression rows.

## Completion gate

Every transition kind meets features_12 phase 2's hold targets. The
full-rebuild, finalization, audit and LtHash equality tests pass
unchanged. `./scripts/check` passes.
