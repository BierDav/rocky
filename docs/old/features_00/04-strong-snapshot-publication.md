# Phase 4: strong snapshot publication

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented with acceptance evidence in [strong publication](../../runtime/strong-publication.md).
This extends the realtime MVP with trusted metadata coordination and one
application-defined strong command. The sections below retain the implementation
brief and acceptance contract.

## Goal and required reading

Publish an immutable todo snapshot at a sealed input cut, with an ordered receipt
from an in-memory coordinator. Demonstrate pending commands, replay-safe retry,
stale-executor fencing and app-node recovery across interrupted confirmation.

Read [MVP scope](README.md), [root instructions](../../../AGENTS.md),
[module boundaries](../../modules.md), [phase 3](03-two-replica-sync.md),
[operation contract](../../protocol/operation-contract.md) and the complete
[strong-command contract](../../protocol/strong-command-contract.md). The latter fixes the
coordination state machine, commitments, lifecycle and recovery limits.

Architecture references: [strong commands](../../proposals/architecture-proposal-v3.md#strong-commands),
[coordinator contract](../../proposals/architecture-proposal-v3.md#6-borrowing-company-coordination-without-uploading-code)
and [application recovery](../../proposals/architecture-proposal-v3.md#7-alices-deployment-and-recovery).

## Ownership and prerequisites

- `rocky-coordinator`: deterministic generic state machine and in-memory adapter.
- `rocky-core`: neutral request/confirmation/receipt and commitment types needed
  on both sides; no todo row types or handler implementation in the coordinator.
- `todo-app`: PublishListSnapshot method, exact-cut replay, result projection and
  caller policy. Its output includes immutable snapshot rows, not live-list edits.
- `rocky-runtime`: durable requests/preparations, submission/reconciliation,
  result acceptance, publication outbox, observer and progress integration.
- `todo-desktop`: compose coordinator and authorized executor, wire the request
  transport, expose Publish and coordination availability/takeover controls.

Preserve crate dependency direction. Runtime integrates the coordinator through
a core/runtime-owned port implemented at composition; it must not import todo-app
or embed todo business logic. The coordinator never reads either app database.

The Laptop starts as executor; Phone can submit a command. The development owner
may explicitly take over to Phone after its confirmed state has caught up.
This is not a failure detector or election protocol.

## Implementation sequence

1. Implement and test coordinator transitions independently of runtime/UI.
   Register fixture domain/configuration/generations and static executor grants.
2. Define the strong contract's exact version-1 encodings and SHA-256 golden
   vectors. Use deterministic bytes for request, package and history head.
3. Add an atomic read for visible rows plus applied frontier. Add canonical replay
   at that frontier; require dependency closure and verify the requested digest.
4. Add durable command IDs/requests and preparation attempts to runtime storage.
   Keep requests distinct from mergeable author logs and ordinary outbox work.
5. Implement executor catch-up, app validation, preparation, external confirmation
   and idempotent receipt/application commit exactly as the strong contract states.
6. Add restart reconciliation for unresolved preparations and confirmed packages.
7. Extend sync with explicit result/receipt messages and missing-input/predecessor
   fetching. Reuse bounded transport and receipt/application acknowledgments.
8. Add the UI flow and scripted failure scenarios after core behavior is tested.

Keep the prepared package immutable. The original request's exact input remains
unchanged across retries even when newer todo edits or a new strong head exist.
For a known head/epoch rejection, reconcile and prepare a new attempt only if the
request has no established outcome. Do not overwrite or discard unknown attempts.

## UI and status contract

Add Publish to each replica panel. It captures that replica's currently applied
cut and persists the request, then routes to the current executor. Display the
immutable publication sequence and rows only after receipt application commits.
Later edits appear in the live list without modifying an existing publication.

Distinguish request states: queued/pending executor or coordinator, prepared,
confirmation outcome unknown, confirmed locally applied, and applied by the named
peer. Failure messages should explain missing input, denied command, stale head
or unavailable coordination without implying rollback of an accepted result.

Disabling coordinator availability leaves ordinary todo editing/sync working.
Prepared or queued publications remain pending. Reenable it to reconcile outcome
and continue; a request retry must not allocate a second publication sequence.

Use the existing run command and data directory. After losing the mock coordinator
ledger on full process restart, follow the explicit strong-contract block instead
of silently initializing a fresh head for a previously coordinated domain.

## Coordinator acceptance checks

- Initial acquire establishes epoch 1; explicit expected-epoch takeover advances it.
- Unauthorized executor/control, unknown domain/configuration and mismatched active
  generations are denied without changing the head or accepted request records.
- Two proposals against one expected head produce exactly one accepted successor.
- Identical accepted retry returns the same receipt with no extra sequence.
- Request ID reused for different input is rejected; a different result commitment
  for an accepted request cannot reuse that receipt to apply a new publication.
- A stale executor cannot confirm new work after takeover.
- An exact retry/historical delivery of a pre-takeover confirmed result stays valid.
- Confirmation objects/state contain only generic metadata and commitments.

## Runtime and result acceptance checks

- Editing during request preparation does not alter the sealed snapshot.
- Executor with newer live edits replays the requested older cut correctly.
- Missing input/predecessor package waits; a mismatched input digest is rejected.
- Prepared/unconfirmed results do not enter final queries or observer snapshots.
- Coordinator unavailability creates pending progress without blocking local edits.
- Receiver rejects unconfirmed/forged mock receipts, hash mismatches, wrong scope,
  unsupported generations and command-policy denial without final materialization.
- Reordered/duplicate result delivery applies each confirmed publication once,
  retaining the strong predecessor chain and separate mergeable frontiers.
- Takeover cannot serve commands until the new executor has the confirmed head;
  recheck after takeover, not only before acquiring it.
- Missing the only copy of a confirmed package blocks strong progress and never
  rewinds the head. Missing coordinator history likewise blocks strong commands.

## Interrupted-lifecycle checks

Use deterministic barriers around each boundary and restart app-node runtime
objects against their DB files while retaining the same coordinator instance.

| Stop/failure point | Required recovery |
| --- | --- |
| Request persisted, before dispatch | Request can be resumed with its original ID/input |
| Prepared locally, before coordinator accepts | Remains unconfirmed; lookup/reconcile before retry |
| Coordinator accepts, reply is dropped | Unknown outcome resolves to original receipt |
| Coordinator accepts, before local receipt/apply commit | Prepared package permits one local application on reopen |
| Failure inside receipt/apply/outbox transaction | No partial final projection; retry whole application |
| Local final commit, before peer publication | Durable outbox/history later supplies package and receipt |
| Receiver stores result, before apply/ack | Reopen resumes verification/application without duplicates |

Verify no database transaction is held at a coordinator barrier. Do not use an
application exception alone to claim physical process/power-loss durability.

## Manual completion scenario

1. Start a fresh directory, synchronize a list and publish from Phone via Laptop.
2. See one immutable numbered snapshot on both peers; then edit the live list.
3. Disable coordination, request another publication and continue local edits.
4. Reenable coordination; the request completes once at its original input cut.
5. Exercise lost reply/retry in the deterministic harness and verify one result.
6. Catch Phone up, explicitly take over, and reject a stale Laptop confirmation.

Run targeted coordinator/runtime/app tests and `./scripts/check`. Launch the app
and document the manual results. Report mock-confirmed versus peer-applied evidence
separately, and note that no signed/quorum-verifiable receipt is implemented.

## Deferred work and handoff

Raft persistence/availability, automatic failover, real credentials, encrypted
packages, production request retention, coordinator disaster recovery and external
effects are later work. Do not turn the mock into an invented consensus protocol.

Update module/project status and document actual coordinator/request ports,
canonical encoding vectors, failure controls and the ledger-loss limitation.
The four-phase MVP is complete only when the acceptance gates above are met;
the broader v3 PoC still requires its security and distributed infrastructure.
