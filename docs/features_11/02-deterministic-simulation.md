# 2. Deterministic simulation

Status: proposed. Owner: `rocky-sdk` integration tests and the in-memory
transport; `benchmarks/chaos` for the multi-process sweep. Depends on
[phase 1](01-see-every-stall.md) for blockers as the progress measure.
Effort: one to two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), finding 13. Every blocker in
the 2026-10-24 device testing came from an ordering of joins, seals,
branch switches, restarts and lossy links that no hand-written test
exercised. The chaos harness (`benchmarks/chaos`) kills real processes and
fuzzes requests, but it is outside CI and has no seals, bookmarks or
compaction.

After this phase a seeded, in-process simulation drives three or four
nodes through random schedules with faults and checks safety at every step
and convergence after healing, in the integration tier.

## Settled

- **In process, single threaded, paused clock.** The simulation runs on a
  current-thread runtime with paused time and the `rocky-sdk` in-memory hub.
  A `u64` seed fixes the schedule; the failing seed and the step list print
  on failure and replay exactly.
- **The hub gains seeded faults.** Per-link latency and jitter, bandwidth
  limits, stalls of a given length, cuts after a byte count, refused dials
  and network changes. Faults are drawn from the seed; nothing reads the
  wall clock.
- **Actions.** Write on any node; pairwise and full sync; sever and heal a
  link; close and reopen a node; crash at a named stage during an import or
  a group commit, through the `qualification` fault hooks; seal and compact;
  open a tip and follow or leave a bookmark; prepare a partition; invite,
  join and revoke. Weights favor the device testing's paths: seals with a
  branch on another node, a member joining after a seal, restarts during
  bootstrap.
- **Safety after every step.** `audit()` passes on every open node; no
  group commit is half applied; every pending frame has a wait reason;
  every blocker has a `clears_by`.
- **Liveness after healing.** All links up, every node follows one
  bookmark, then at most K sync rounds (K fixed per scenario size). Pass
  means: no blockers, no staged imports, no bootstrap marker, no pending
  frames, and identical application tables on every node. The number of
  blockers must not grow across two consecutive healing rounds; if it does,
  the run fails and dumps the blockers.
- **Tiers.** A fixed seed set runs in the integration tier within the
  existing per-test and suite budgets, with no exception. A sweep sized by
  `ROCKY_SIM_SEEDS` runs in e2e. Until phase 4 lands, liveness failures are
  reported and recorded as known seeds, while safety failures fail the run.
- **Chaos covers what processes add.** `benchmarks/chaos` gains seal,
  bookmark and compaction actions in its fuzzer and runs nightly with a
  stored seed range; it remains its own workspace.

## Ordered slices

1. Seeded faults in the hub, with tests that the same seed produces the
   same transcript.
2. The simulation driver, actions and safety checks.
3. Healing and liveness checks; the known-seed list for liveness.
4. Mutation check: revert two device fixes in a scratch build (reset replay
   applying pending frames; recovery skipping an unverifiable proof) and
   record that the e2e sweep finds a failing seed for each.
5. Chaos fuzzer actions and the nightly job.
6. Evidence: seeds run, runtime per tier, known liveness seeds and their
   blockers.

## Completion gate

- The same seed yields byte-identical hub transcripts on two runs.
- The integration seed set runs inside its budgets; the e2e sweep runs at
  least 1,000 seeds within the e2e suite budget or its recorded exemption.
- The mutation check finds both reverted fixes.
- No safety failure in the sweep on the phase's final revision.
- The chaos fuzzer reaches seal, follow and compact actions in its
  schedule log.
