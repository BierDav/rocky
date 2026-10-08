# Phase 6 — Scaling, upgrades and SDK qualification

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: partially implemented after [phase 5](05-recovery-and-effects.md).
[Bounded encrypted history access, redundant archive retention and same-definition
shadow rebuilds](../../runtime/scaling-and-rebuilds.md), plus a [JSON SDK and real
Kotlin/JVM consumer](../../runtime/sdk-contract.md), have [native/JVM evidence](../../runtime/scaling-evidence.md).
Destructive checkpoint compaction, generation migration and mobile/platform gates
below remain outstanding. V3 references: sections 8, 10–14.
Deliver these as separate milestones with their own evidence.
The checkpoint and pruning work below continues in the
[scale plan](../features_02/04-checkpoints-and-pruning.md).
Note 2026-10-04: Turso references below predate the switch to SQLite
([decision 0007](../../decisions/0007-sqlite-engine.md)).

## Scaling and retained history

Distribute independently ordered domains across separately credentialed app nodes;
a Kubernetes replica is not automatically an executor or full data replica.
Measure one shared coordinator group, noisy-client quotas and domain/query/observer
contention before introducing more groups. Establish explicit memory, frame,
queue, transaction, snapshot and WAL/log-reclamation budgets.

Compact using verified checkpoints and a declared old-client horizon. Preserve
anti-resurrection evidence, unresolved proposals, deduplication summaries, pending
jobs and sole recovery copies. Pin data until declared custody permits deletion.
Scope eviction, tombstones and key retirement remain separate operations. Replace
exact full-history scans with bounded retained ranges/checkpoints without skipping
late commits or confusing receipt with application.

Gate: old offline replicas either converge safely or require explicit rebootstrap;
replayed compacted requests/effects do not execute again. Missing custody prevents
unsafe pruning. Report same-file independent-domain and observer tail-latency
results against explicit workloads, following benchmark instructions if changing
that workspace. More coordinator groups require demonstrated contention and
recorded assignment/routing/configuration rules.

## Generations and upgrades

Publish signed compatibility transforms before enabling new writers. Upcast old
operations deterministically while retaining original signed bytes. Rebuild local
projections in a shadow schema, validate/catch up and switch atomically.

Change strong state/invariants through an application migration result whose head
and handler/schema activation are confirmed together. A generation bump alone
cannot reinterpret the existing head. Keep policy/encryption/configuration versions
independent and pending business requests explicit under lossy transformations.

Gate: mixed old/new clients, interrupted rebuild/activation, incompatible executor,
pending requests and old checkpoints have explicit safe outcomes. Verify no
head rollback, signature rewriting or resurrection.

## SDK and engine/platform qualification

First stabilize a language-neutral contract and error/progress encoding; then add
Kotlin suspend/Flow bindings with cancellation, lifetime, coalescing and dynamic
bound queries. Expose operation/request handles, scoped subscriptions, recovery
and explicit local-versus-strong reads. Generated typed models consume the same
precise mutation contract; they cannot use unrestricted full-row ORM updates.

Qualify an optional Room read adapter against actual Turso driver/SQL APIs.
Rocky owns migrations and writes. Test one Native/KMP target independently; mobile
suspension and data-path lifecycle are availability constraints. Web/WASM requires
its own storage/crypto/transport qualification and is not implied by Kotlin support.

Only then qualify an optional managed SQL write bridge: constrain supported shapes,
protect protocol tables, own transaction completion, authorize before/after changes
and preserve remote-origin suppression. Optional WAL CDC has a separate committed
capture/cursor/retention gate; full MVCC CDC remains off by default.

Gate: real Kotlin consumers exercise observation races, cancellation, slow collectors,
mutation/precondition errors and recovery. Room cannot independently migrate/write.
Platform builds and on-device process restart tests accompany package tests and
`./scripts/check`. Publish supported versions/targets and remaining limitations;
no blanket SQLite/Turso/Room or all-features compatibility claim.
