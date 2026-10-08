# Modular P2P infrastructure implementation plan

Status: implemented; accepted 2026-10-06, superseded as next-work authority by
[features_05](../features_05/README.md) on 2026-10-07. Phases 1 to 8 are implemented.
Phase 9 is qualified on loopback with [recorded limits](../runtime/end-to-end-evidence.md);
its real-network evidence is outstanding and remains a features_04 item.
Read [architecture V4](../proposals/architecture-proposal-v4.md) and
[decision 0019](../decisions/0019-composable-p2p-and-public-coordination.md).
Historical phase folders document foundations
and evidence; they do not impose a competing future sequence.

## Destination

An app composes database/replication, credentials and explicit typed strong
commands. Networking is optional: attach Rocky services to an app-owned iroh
endpoint or use the starter setup. Your own RPC protocols coexist independently.
Consumers define schemas and handlers in their own language and use packaged
runtime artifacts without a Rust project or toolchain, through in-process UniFFI
bindings ([decision 0020](../decisions/0020-in-process-uniffi-bindings.md)).
Every capability has a
language-neutral contract and an ORM/transport adapter boundary; the first shipped
consumer qualification is Kotlin/JVM desktop, not universal adapter availability.
The starter selects the provider's public metadata coordinator; schemas, handlers
and application payloads remain on app nodes. Coordinator storage is bounded by
live domains/sessions and configured windows, not total lifetime commands.

## Sequence

| Phase | Agent brief | Depends on | Required result |
| --- | --- | --- | --- |
| 1 | [Service contracts and composition](01-service-contracts-and-composition.md) | Existing foundations | Public boundaries, resource ownership and a compatibility plan |
| 2 | [Optional iroh plumbing](02-iroh-and-network-ownership.md) | 1 | App-owned endpoints, independent database lifetime and direct peer sync |
| 3 | [Credentials and enrollment](03-credentials-and-enrollment.md) | 1, 2 | Independent credential module with runtime enforcement and scoped enrollment |
| 4 | [Database queries and observation](04-database-queries-and-observation.md) | 1; 3 for secure materialization | ORM-neutral managed schema/session/read/event APIs |
| 5 | [Typed strong commands](05-typed-strong-commands.md) | 1–4 | Typed invocation/results, application-language handlers and independent delivery |
| 6 | [Bounded coordinator retention](06-bounded-coordinator-retention.md) | 1, 5 | Safe retirement, verifiable historical evidence and plateauing storage |
| 7 | [Public coordinator service](07-public-coordinator-service.md) | 2, 3, 5, 6 | Isolated registration, bounded operation and backend-free starter configuration |
| 8 | [Recovery, upgrades and SDKs](08-recovery-upgrades-and-sdks.md) | 3–7 | Recoverable generations, composable native/Kotlin APIs and packaged starter |
| 9 | [End-to-end qualification](09-end-to-end-qualification.md) | 1–8 | Reproducible P2P app and public-service acceptance evidence |

Implement in this order by default. Bounded slices can overlap only after their
prerequisite contracts and evidence are fixed. Trusted-profile work in phase 4
may run alongside phases 2 and 3; its secure-profile slices wait for phase 3.
Phase 5 is the largest novel engineering item and should be budgeted as such.
Do not start public deployment
before phase 6 passes. Every phase delivers usable behavior, not empty crates.

## Foundations and old-plan disposition

`features_03` phases 1–4 supplied logical schemas, precise SQL capture, bindings
and encrypted bundles/sessions. Keep their recorded limitations: secure typed
storage still differs from the trusted profile, whole-row read scopes remain,
and enrollment/key/Internet gaps are not resolved by marking those phases done.
The current module map is [modules](../modules.md).

| Old unfinished brief | New ownership |
| --- | --- |
| features_03 phase 5 | Generic reads/events in phase 4; Room catalog/tracker adapter deferred |
| features_03 phase 6 | Generic checkpoint/migration/recovery in phase 8; Room import/callback adapter deferred |
| features_03 phase 7 | Explicit commands/effects in phase 5; transparent strong DAO writes deferred |
| features_03 phase 8 | Composition in phase 1, network in 2, credentials in 3, bindings/package lifecycle in 8 |
| features_03 phase 9 | Runtime/service fault and scale evidence in phase 9; Room trace/driver qualification deferred |
| features_01 unfinished phase 6 | Retention in 6, upgrades/SDKs in 8; mobile scope remains deferred |
| features_02 unfinished qualification | Preserve budgets and evidence; run applicable gates in phase 9 |

The old Room recording fixtures and briefs remain useful acceptance references
when that adapter resumes. They are not prerequisites for generic Rocky release.
No phase claims universal SQL/ORM support, arbitrary cross-file ACID, strong
Kotlin transaction closures or compatibility with every existing app.

## Agent execution contract

Give an agent this README plus one linked phase brief. Read root `AGENTS.md`,
[development](../development.md), V4 and the phase's current source/guide inputs.
Verify prerequisites against code and evidence before editing. If a prerequisite
fails, repair it within scope or report the exact blocking contract; never invent
successful behavior or silently weaken admission, durability or atomicity.

Product choices the briefs once left open are settled in [resolved choices](resolved-choices.md);
do not reopen them. Select internal mechanisms inside the accepted boundary. New persisted/wire
formats need a focused decision record, versioning and an upgrade/refusal path.
Illustrative API names in proposals are not implemented APIs. Update owning
guides, this phase's status and measured evidence with each completed slice.

Use `jj` only; preserve unrelated work and leave edits in the current change.
Run targeted package checks and `./scripts/check`. Native/Kotlin/platform and
Internet checks are additional when relevant. Consumer use must not require those
Rust/Python development tools; contributors still use them to build the runtime.
Faults use deterministic hooks; time-dependent tests use controllable clocks.
Report pass/fail and limits.

Existing [scale budgets](../old/features_02/README.md#target-budgets) remain targets,
with [current misses](../runtime/scale-qualification.md) visible. Set coordinator
count/byte/latency budgets before performance work; do not raise quotas or relax
targets silently. Check supported feature combinations rather than all-features.

## Release gate and deferred integrations

Phase 9 must demonstrate two real persistent app processes with mergeable offline
edits, a typed strong transition using a provider-style shared coordinator, safe
retry/expiry, independent networking lifetime and distinct public/private data,
using an application with no Rust source/build.
Long-run coordinator state must plateau and churn must not build a lifetime ledger.

Public-service software readiness is separate from permission to deploy it.
No phase authorizes publishing credentials, pushing changes or starting a paid
public service. Provider setup/runbooks make an eventual deployment reviewable.

Optional later work: Room driver/trace qualification, login-provider integrations,
additional engines, generalized discovery, full blob/presence services, mobile/web
qualification, transparent declared strong DAO bindings and cross-coordinator
migration. None is implied by this release or by the historical feature wishlist.
