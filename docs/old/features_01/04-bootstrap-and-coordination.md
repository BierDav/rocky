# Phase 4 — Bootstrap, directory and shared coordination

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented as a bounded native profile after [phase 3](03-permissions-and-sync.md).
See [shared coordination](../../runtime/shared-coordination.md),
[Directory policy](../../applications/directory/registration.md),
[durability decision](../../decisions/0003-certified-raft-coordination.md) and
[multiprocess qualification](../../runtime/coordination-evidence.md). V3 references:
sections 2, 4, 6 and 8. `rocky-coordinator` owns generic metadata; a directory
application owns registration policy; runtime owns client command recovery.

## Ordered slices

1. **Root-certified genesis.** Bind mesh identity, initial coordinator membership/
   configuration, directory definition/bootstrap domain and bounded administrative
   grants. Authorize the initial directory executor without relying on an already
   running directory. Keep the root signing key offline after bootstrap.
2. **Directory application.** Implement user/deployment/node registration,
   ownership and bounded delegation through the same application command API.
   Registration must not grant another app's data rights. Include user deletion:
   explicit domain ownership disposition, node/grant revocation, keys and pending
   work handling, with learned-revocation/offline limits. Do not introduce universal
   framework CEO/admin roles.
3. **Durable coordinator state machine.** Persist configuration, authorized
   control references, executor/epoch, active generations, head/sequence and
   deduplication. Choose an established Rust Raft implementation after a focused
   integration spike and record its durability/membership model. Keep application
   code, policy predicates and business payloads outside the service.
4. **Network coordination.** Implement authenticated async acquire/takeover,
   head, confirm and lookup operations with stable request IDs. Bind trusted
   signed receipts to scope/configuration, epoch, generations, request/result
   commitments and predecessor. Receipt verification can use retained trusted
   evidence during outages. Do not require a quorum-proof/BFT system.
5. **Three-voter exercise.** Run independent persistent voter processes and one
   Alice app executor. Confirm under quorum, survive coordinator leader loss,
   reject stale app executors and recover through snapshots/log replay. Directory
   and Alice use the same generic service without loading Alice's code there.

## Gate

Bootstrap from empty state with an offline root afterward. Register Alice and a
client-only phone using distinct node credentials. Demonstrate that registration
cannot write Accounting; only an Accounting-issued bounded grant can do so.

Kill/restart voters at log/commit/snapshot boundaries. Duplicate confirmations
return the historical outcome; request-ID misuse fails. Quorum loss leaves strong
commands pending while permitted mergeable edits work. A newly empty coordinator
must never masquerade as the authority for an existing domain.

Test membership/configuration changes, same-holder fencing, takeover during
execution and historical receipt delivery. Require durable Raft evidence before
claiming coordinator-confirmed success. Measure request/log quotas and describe
exceptional permanent-quorum recovery authority. Bounded deduplication retention
is completed with verified checkpoints in phase 6; preserve all required records
until then. Run package checks, `./scripts/check` and the multiprocess scenario.

## Delivered gate

Root-certified bootstrap starts three independent persistent OpenRaft voters with
no online root secret. Directory registers Alice and distinct executor/phone keys
through authenticated command ingress; a client-only phone cannot acquire execution
or certify another application's control. A real Directory executor and Alice's
Inventory runtime use the same generic service. Accounting requires its own bounded
signed grant; Directory registration alone fails.

The process exercise covers lost leader, durable log/commit/application/reply
boundaries, generated snapshots and interrupted incoming snapshot installation.
Retry records survive replay and compaction, stable-ID misuse fails, same-holder
fencing and takeover reject stale preparations, and signed historical evidence
verifies with all voters offline. Membership and control revisions preserve history.
Quorum loss leaves strong work pending while admitted mergeable edits commit.

Limits are explicit: trusted crash-fault voters/executors, native loopback
qualification, whole-image fsync storage, root-certified candidate/scope bounds,
no automatic permanent-quorum reset and no retention pruning. Directory deletion
records explicit domain disposition and revocation/key/pending-work plans;
application owners publish their own revocations and rotate keys. Offline disclosure
cannot be undone. The owning guides describe these boundaries and exceptional
recovery authority rather than claiming production/BFT or power-loss qualification.
