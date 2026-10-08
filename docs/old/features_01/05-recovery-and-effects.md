# Phase 5 — Replica recovery and application effects

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented for the [bounded encrypted recovery/effects profile](../../runtime/recovery-and-effects.md) after [phase 4](04-bootstrap-and-coordination.md). The guide records APIs, fault evidence and remaining deployment limits. V3 references:
sections 7, 12 and 14. Runtime owns recovery artifacts and job lifecycle; apps own
job meaning and downstream authorization; coordinator supplies fencing/metadata.

## Recovery slices

1. Define canonical encrypted checkpoint/recovery packages binding definition,
   domain/scopes, generations, policy/key epochs, operation frontier, strong head,
   dependency/authorization evidence, deduplication, unresolved requests and jobs.
   A SQL export is not a recovery package. Export coherent state without a long
   write transaction; inspect hashes and custody coverage before import.
2. Import onto a fresh authorized node without copying an old signing identity.
   Verify payloads/receipts and compare with coordinator head. Fetch missing
   packages; reconcile unknown outcomes and preserve offline edits.
3. Fence the old executor through takeover, then recheck/recover the head again.
   Commands resume only with the required confirmed state and handler generation.
4. Add explicit replacement-domain recovery when the newest confirmed payload is
   lost. Preserve the old audit identity, fence/close its execution role and report
   acknowledged loss/uncertain effects. Never rewind the old confirmed head.
5. Add optional recovery-custodian acknowledgments covering the package and full
   required dependency closure before confirmation. Default async peer replication
   remains a distinct, weaker durability policy.

## Application extension slices

Create Alice's app-owned checkbox/job schema joined to authorized Accounting
events. An Accounting grant authorizes only declared reads/field updates;
Alice's receipt cannot authorize arbitrary Accounting changes. Current business
conditions are checked by commands at the resource-owning app, with explicit
cross-app workflow/freshness semantics rather than atomic cross-app transactions.

A deterministic transition records stable job IDs. Execute webhooks outside
storage/coordinator locks and only after valid confirmation where required.
Record attempts/outcomes as authenticated observations, retry with provider
idempotency, and represent send-success/record-failure uncertainty. Record time,
randomness, human/AI choices and external responses as inputs, never replay them
by making the external call again. Old-executor fencing cannot retract an already
started webhook.

## Gate

Kill the PC before confirmation, after confirmation before local application and
after publication. Recover from phone onto a new credential and serve commands.
Withhold the newest package: old-domain recovery blocks; explicit replacement is
visible and never presented as transparent failover. Recheck the head when an old
executor advances during import. Lost data keys produce a recoverable/blocked
state, not a fabricated empty database.

Test repeated checkpoint import, mixed pending/confirmed work, dependency closure,
private companion exclusion and custody loss. Crash before/after webhook sending
and outcome recording; show possible duplicate/uncertain delivery and stable job
identity. Exercise grant revocation and downstream denial. Run `./scripts/check`
plus actual recovery/network/effect scenarios; report local durability,
coordinator confirmation and recovery replication separately.
