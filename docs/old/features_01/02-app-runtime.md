# Phase 2 — App-owned runtime and developer API

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented with bounded adapter qualification after [phase 1](01-audit-fixes.md).
See [implemented contracts](../../runtime/app-runtime.md) and
[regressions, measurements and limits](../../runtime/app-runtime-evidence.md).
V3 references: sections 3, 5, 10–11 and 13. Owners: core protocol/application
contracts, runtime storage/scheduling/query/commands, application-owned handlers.
Note 2026-10-04: built on Turso MVCC; the engine is now SQLite with one writer
per file ([decision 0007](../../decisions/0007-sqlite-engine.md)).

## Deliver in this order

1. **Application definition and namespace.** Define the language-independent
   manifest body with stable publisher-bound app/entity/field/method IDs,
   immutable hash, operation shapes, domain rules and separate schema/projection/
   handler versions. Bind installed handlers to supported hashes/generations;
   receiving a manifest never installs executable code. Signing follows phase 3.
2. **Same-file domains.** One runtime owns the file and several domain workers;
   independent connections use MVCC while domain/author/crypto conflicts serialize.
   Persist scoped revisions/counters/history. Allocate operation IDs per declared
   author log scope and include that scope in identity/causal references. Qualify
   real conflicts and retry the whole transaction with stable operation identity.
3. **General strong commands.** Requests bind method/version, arguments, caller,
   stable caller-supplied request ID, policy reference and exact inputs. Handlers
   produce deterministic decisions, projection deltas and job intents against the
   recovered strong predecessor plus the sealed mergeable input cut. Retain
   rejected business decisions as explicit outcomes. Durable preparation and
   confirmation remain separate from application; local drafts remain independent.
4. **Queries and observation.** Add bound read queries/read options and a supported
   typed query facade. Reject writes, DDL, ATTACH and protocol-table access through
   reads. Queries execute over the authorized local subset and return local scope
   revision/completeness. Observers invalidate relevant tables/scopes and coalesce
   bounded snapshots. A query does not create a remote subscription.
5. **Strong reads and tracking.** Bind coordinator head discovery to fetching and
   applying the matching payload. If dependencies are missing or coordination is
   unavailable, return a precise pending/unavailable result. Track operation and
   command handles separately from query rendering; identify peers explicitly.

## API contract to demonstrate

Illustrative responsibilities, not fixed Rust signatures:

| Boundary | Developer supplies | Runtime returns/owns |
| --- | --- | --- |
| Open | Installed definition/manifest, node configuration, storage policy | Runtime lifecycle and compatibility errors |
| Mutate | Entity/action or precise patch and optional baseline | Stable operation handle, atomic durable commit |
| Command | Domain, method, arguments, request ID and exact inputs | Durable request handle and recorded outcome |
| Query/observe | Bound query, scope/read options | Authorized rows, committed revision and completeness |
| Execution | Authorized domain assignment and coordinator port | Fenced execution and reconciled confirmed head |
| Track | Accepted operation/request handle, named peers | Coalesced evidence-based progress |
| Close | Lifecycle request | Drained accepted local work and released resources |

App authors implement deterministic reducers, admission and registered command
handlers. UI authors consume typed facades/handles and never construct SQL writes
or strong packages. Keep engine types and fixture defaults out of the supported
facade. Publish compiling examples for both audiences and explicit error/cancel
contracts. Do not add traits without an actual application/transport replacement.

## Gate and verification

Build a second non-todo app, such as inventory: independent draft notes merge
locally while stock reservation is a strong command. A reservation changes state
and records a job intent; snapshot publication alone cannot satisfy this gate.
Do not treat a current-state-dependent stock decrement as qualified mergeable.

Demonstrate two independent domains concurrently mutating one file, conflicting
same-domain commands ordered, atomic rollback/retry and no global revision hot
row. Test duplicate request IDs with same/different arguments, recorded rejection,
reordered results, missing inputs, cancellation and concurrent local draft edits.

Qualify dynamic queries, source completeness, scope invalidation and strong reads
with a deliberately newer coordinator head. Measure runtime throughput, tail
latency, memory, snapshot lifetimes and log growth for independent/conflicting
workloads; benchmark evidence alone does not qualify this adapter. Benchmark
changes follow [benchmark instructions](../../../benchmarks/engine-comparison/AGENTS.md).
Run `./scripts/check`; report serial/single-domain fallback limits explicitly.
