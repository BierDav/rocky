# 7. Bounded transitions

Status: deferred; starts only on the trigger below. Owner:
`rocky-secure`: checkpoint installation, rebootstrap, live schema switch,
projection rebuild, compaction, finalization and tip workspace rebuilding.
Depends on [phase 6](06-qualification.md). Effort: not estimated until
the design slice lands. Follow the [execution contract](README.md#agent-execution-contract).

## Why it is separate

A transition replaces a scope's rows, projections and history in one
SQLite transaction, so readers never see a mixture and a crash rolls the
whole replacement back. Its work grows with the scope. The phase 2
[inventory](02-bounded-holds-evidence.md) showed that existing staging
can't make that replacement visible in one bounded step. The projection
plan's final switch also verifies every row and builds indexes. Phase 1
recorded holds of 2.4 s (desktop checkpoint installation, 100k rows),
39.5 s (Pixel installation) and 89.1 s (Pixel L3 compaction).

Transitions are rare compared with interactive writes. Until this phase
runs, phase 3 keeps them from starting while interactive work is waiting.
They still block writes once they start.

## Trigger

Start this phase only if one of these holds:

- Phase 6 shows a transition overlapping interactive writes in the
  shopping app scenario, or in a recorded real workload, with a write
  above the README's max targets.
- The product decides that bootstrap, rebootstrap or compaction must
  never stall local writes.

## Settled

- **Design first.** Slice 1 is a reviewed design, not code. It must
  establish everything listed under "What a solution must establish" in
  the [phase 2 evidence](02-bounded-holds-evidence.md#what-a-solution-must-establish).
  That covers the stable source cut, invisible staging, handling of
  writes during preparation (with a liveness argument), bounded
  verification and index work, a bounded publication transaction,
  recovery states and bounded cleanup.
- **Format change allowed.** A generation selector and a durable install
  journal are the expected candidates. If this phase overlaps phase 5,
  the change shares phase 5's single bump. Otherwise it is one new replica
  version, with no reader for the earlier one. Don't overload existing
  cursors or flags with new meanings.
- **Same guarantees.** Checkpoint commitments, identity and provenance
  checks, handle retries, revision and flush ordering, and audit results
  keep their meaning.

## Ordered slices

1. Design, recorded in evidence, with the format decision.
2. Implementation per the design, one transition kind at a time.
   Checkpoint installation comes first.
3. Tests: interrupt after every step and compare the resumed rows,
   projections, operation statuses, identities, logs, base summaries and
   audit results with an uninterrupted run. Interleave local writes,
   incoming frames, grants and revocations, and tip changes at
   deterministic boundaries. Count all work per acquisition, including
   verification, index work, replay and cleanup.
4. Rerun the L1–L4 probe and the sync non-regression rows.

## Completion gate

Every transition kind meets phase 2's hold targets. The full-rebuild,
finalization, audit and LtHash equality tests pass unchanged.
`./scripts/check` passes.
