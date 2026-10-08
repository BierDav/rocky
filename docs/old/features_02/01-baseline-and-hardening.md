# Phase 1 — Baseline, harness and hardening

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-04; budgets confirmed and recorded in the
[plan](README.md#target-budgets). Evidence: [scale baseline](../../runtime/scaling-baseline.md),
[encrypted baseline](../../runtime/encrypted-baseline.md) and
[decision 0006](../../decisions/0006-durability-classes.md).
Prerequisite for every other phase in the [plan](README.md).
Context: [scaling limits](../../runtime/scaling-limits.md) and its probe,
`benchmarks/runtime-scaling/src/bin/scaling_probe.rs`.
Note 2026-10-04: written and measured on Turso 0.8.1. [Phase 2](02-sqlite-engine.md)
replaces `BEGIN CONCURRENT` and `StorageThreads::PerDomain`
([decision 0007](../../decisions/0007-sqlite-engine.md)).

The existing probe uses one small table. Later phases need a workload at target
size, a measurement of the encrypted path and prototypes that are safe to build on.

## Slices

1. **Confirm budgets.** Present the [target budgets](README.md#target-budgets) to
   the user with the current measured value beside each. Record the agreed numbers
   in the README and do not start phase 2 without them.
2. **Wide probe application.** Add a probe-only application with a generated
   schema: configurable table count up to 2,000, a few indexed columns per table,
   creates, field updates and deletes. Keep it out of the supported API. Add probe
   sections that load a domain to a configurable row count and report write, point
   read, range read, observer, open and memory figures at several sizes. Sections
   must report a refusal as a result, not abort the run.
3. **Encrypted path baseline.** Add equivalent sections for `EncryptedReplica`:
   authoring, incoming application, reads, transfer pages, rebuild and open. The
   replica serializes all work behind one mutex and has its own 4,096-record and
   32 MiB quotas; measure what that costs before changing it. Record the results
   in a new section of the scaling limits guide.
4. **Whole-transaction retry everywhere.** Every `BEGIN CONCURRENT` in the trusted
   strong path and in incoming replication must sit inside a bounded retry of the
   entire transaction, as local mutation already does. Use one shared helper.
   Reproduce the `database is locked` failure under `StorageThreads::PerDomain`
   first, determine its trigger in the pinned engine and record it.
5. **Crash coverage for prototyped options.** Group commit: kill the process
   mid-group and verify that on reopen each handle is either fully committed or
   absent, and that sequence, clock, revision and projection agree with history.
   Read cache: verify invalidation across incoming application, strong result
   application and projection rebuild, not just local mutation.
6. **Durability classes decision.** Write a decision record that states which data
   may use `Durability::Fsync` or weaker and which may not. Strong results,
   confirmations and recovery custody stay at full durability. The record must
   state what an acknowledged-then-lost local write does to peers that already
   received it and to the author sequence after restart.

## Gate

- The wide probe loads as many rows as today's quotas allow at 2,000 tables and
  reports the refusal beyond that.
- Encrypted-path figures are recorded with the same honesty as the trusted ones.
- Strong commands and incoming replication pass on two domains under
  `PerDomain` threads for at least 10,000 operations without a storage error.
- Crash tests for group commit pass repeatedly.
- Budgets and the durability decision are recorded. `./scripts/check` passes.

## Out of scope

Changing quotas, the state model or any storage format. This phase only measures
and hardens.
