# Architecture V3 implementation plan

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: phases 1–5 implemented after the source/API audit on 2026-10-03 at MVP
parent `8dd33dd0`. Phase 3 has a [bounded encrypted native profile](../../runtime/permissions-and-sync.md);
Phase 4 has [persistent three-voter qualification](../../runtime/coordination-evidence.md);
Phase 5 has [bounded recovery/effect qualification](../../runtime/recovery-and-effects.md);
phase 6 has [partial native/JVM milestone evidence](../../runtime/scaling-evidence.md);
its checkpoint, generation and mobile gates remain outstanding. Phase 2 has [bounded adapter qualification](../../runtime/app-runtime-evidence.md). See
[phase-1 resolution evidence](audit.md#phase-1-resolution-evidence). The [V3 proposal](../../proposals/architecture-proposal-v3.md) is the historical
reference for these foundations; the [MVP plans](../features_00/README.md) describe the smaller,
trusted demo that already exists. Engine references in these briefs describe
Turso 0.8.1, replaced by SQLite on 2026-10-04 ([decision 0007](../../decisions/0007-sqlite-engine.md)).

Start with the [audit](audit.md), then complete **all phase 1 findings before
starting feature expansion**. Signed admission and encryption now use the explicit
secure profile. Raft and Directory now have a bounded native implementation; the original trusted
MVP did not require them.

## Sequence and gates

| Phase | Brief | Completion gate |
| --- | --- | --- |
| 1 | [Fix the audit findings](01-audit-fixes.md) | F01–F12 resolved, with regressions and an honest supported API contract |
| 2 | [App-owned runtime and developer API](02-app-runtime.md) | A second app, multiple domains in one file, constrained queries and general strong transitions work |
| 3 | [Permissions and opaque synchronization](03-permissions-and-sync.md) | Signed manifests/grants, encrypted row scopes and atomic p2panda integration pass denial/crash checks |
| 4 | [Bootstrap, directory and shared coordination](04-bootstrap-and-coordination.md) | Root bootstrap plus three durable coordinator voters serve independent app executors |
| 5 | [Recovery and application effects](05-recovery-and-effects.md) | Fresh-node recovery, explicit missing-payload loss handling and a webhook workflow pass fault checks |
| 6 | [Scaling, upgrades and SDK qualification](06-scaling-upgrades-and-sdks.md) | Bounded retention, generation upgrades, Kotlin bindings and measured platform support |

Each phase can be delivered in the ordered slices in its brief. Keep phase 3
security and phase 4 consensus qualification explicit; in-process tests do not
establish either. Preserve the runnable todo demo throughout the transition.

## Implementation workflow

Read [AGENTS.md](../../../AGENTS.md), [module boundaries](../../modules.md) and
[development](../../development.md). Verify the prior gate against code and tests.
Use Jujutsu exclusively and leave edits in the current change unless asked to
publish. Record substantial architectural decisions under
[decisions](../../decisions/README.md), with the chosen formats/dependency versions
and compatibility consequences.

Each delivery updates its owning guide, adds meaningful fault/invariant checks,
runs targeted package tests and `./scripts/check`, and reports remaining limits.
An API example must compile against the actual supported boundary. Do not declare
an entire phase complete from the presence of traits or successful baseline checks.

## Audit evidence

The baseline `./scripts/check` passed: formatting, Clippy, 52 tests and doc tests.
Two temporary integration probes also passed by demonstrating the failures in
F01 and F03. They were removed after the audit; phase 1 must add permanent
regression tests asserting the corrected behavior. Other findings are source
analysis, with proposed verification in their briefs. No native UI, network,
power-loss, mobile or distributed consensus qualification was run for this audit.
