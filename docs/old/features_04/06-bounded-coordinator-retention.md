# 6. Safe bounded coordinator retention

Status: implemented native profile, 2026-10-06; mandatory before public launch. Owner: coordinator state
machine/storage/client evidence and runtime ticket recovery. Depends on phases
1 and 5. Follow the [execution contract](README.md#agent-execution-contract).

Implemented protocol: [decision 0026](../decisions/0026-bounded-coordinator-retirement.md).
Recovery contract: [guide](../runtime/coordinator-retention.md).
Completion evidence and qualification limits: [bounded-state evidence](../runtime/coordinator-retention-evidence.md).
The fixed certified scope universe is bounded; dynamic public registration remains phase 7.

## Inputs and objective

Read [shared coordination](../runtime/shared-coordination.md),
[coordination evidence](../runtime/coordination-evidence.md), current `Domain.accepted`,
metadata mutation/control retry tables, Raft snapshots/store and client evidence.
Before this phase, snapshots retained all command/retry records and hit quotas; neither
compaction nor raising quotas meets this phase.

At fixed live domain/session counts and configured limits, coordinator disk and
memory must plateau regardless of total commands or elapsed application lifetime.
Expired devices, closed domains and administrative churn must not create a second
unbounded history. Full historical lookup is replaced by a published recovery
contract; application replicas retain long-term results and signed evidence.

## Protocol decision before implementation

Record a versioned retirement protocol and explicit storage formula. At minimum:

`state <= live_domains * domain_budget + live_sessions * session_budget + bounded_response_bytes + bounded_consensus_storage`

Budgets include count and bytes for heads, current controls, identities, response
windows, outstanding holes, key/configuration trust evidence and pending work.
Define numeric defaults and per-tenant maxima in the decision before measuring.
Retention age alone is not a capacity bound under unbounded traffic.

Prove the per-session sliding sequence window and initial budgets from
[resolved choices](resolved-choices.md). Keep compact retirement floors and bounded unresolved holes;
retire a prefix only under the selected safe rule. Coordinator sequencing must
not accidentally reuse a p2p author counter shared across unrelated domains.
Retired ranges reject all old identities, including previously unconfirmed ones.

Full response retention, deduplication/replay prevention and payload custody are
three different guarantees. A compact floor prevents execution of an old identity
but cannot reconstruct its exact result. Never interpret a missing response as
proof that it failed, or automatically allocate a new business request identity.

## Ordered slices

1. Introduce bounded admission windows with authenticated session/domain binding,
   immutable input hashes and explicit lifetime. Define control/membership/upgrade
   operation replay protection too, not just business commands. Persist safety
   state in the same consensus transition as acceptance/retirement.
2. Define protocol outcomes: retained confirmed result, authoritative not-confirmed
   evidence under a closed admission window, retired/expired identity with outcome
   unavailable, input conflict and current pending/unknown. Live lookup absence
   alone cannot exclude a later in-flight confirmation. Update runtime recovery
   so it never prepares a retired request again.
3. Define renewal and expiry using controllable time with a safe authority-clock
   model, including backward jumps, restart and leader failover. Old session/domain
   tokens must remain invalid after their records are removed. Re-registration
   requires fresh identity/incarnation; copying an expired token cannot resurrect
   it. Avoid an unbounded tombstone/revocation list as the replacement design.
4. Bound full receipt retention and offline pinning. Durable client acknowledgment
   can enable early retirement; the published maximum recovery horizon still
   expires abandoned work. Outstanding records consume quotas and backpressure;
   they cannot reserve public storage forever. Ticket expiry does not imply rollback.
5. Retain verifiable historical receipts on app nodes. Preserve enough bound trust
   evidence for offline verification after voter/control changes without retaining
   every historical voter/configuration in server state. Proof/evidence formats
   and maximum sizes must be explicit; a result without evidence proves nothing.
6. Compact all state-machine indexes, retry/control records, consensus logs,
   snapshots and persistent allocation. Snapshot swaps and crash recovery preserve
   retirement floors atomically. Bound snapshot generation/transfer and disk peaks;
   count live bytes and actual on-disk bytes, not just map entries.
7. Define domain closure, inactivity policy and safe recreation. Closed domains
   never resume at genesis under their old certified identity. Active retained
   domains keep heads even if quiet. Provider backup/log retention inherits bounds.

## Completion gate

- Drive at least ten complete retention windows at fixed domains/sessions with
  deterministic time. Report stored records, snapshot/log/disk bytes, memory,
  compaction peak and latency; each settles inside the selected fixed budgets.
- Repeat with device/session/domain churn and administrative mutations; no
  per-lifetime device/domain/control/tombstone/configuration ledger accumulates.
- Retry immediately before/after retirement, including lost response and crashed
  client/executor. Outcomes remain honest; old identities never execute again.
- Test reordered concurrent requests, gaps, changed input, session renewal/expiry,
  clock rollback, voter failover, crash during retirement/snapshot and reopen.
- Historical signed results still verify after server retirement; missing evidence
  reports unavailability. No cached receipt can reset current authoritative heads.
- Update old retain-all files/protocol clients through migration or explicit
  refusal. Existing fencing, confirmation and custody invariants continue passing.

Run coordinator/runtime/secure fault suites, the bounded-state probe and
`./scripts/check`. Publish the chosen recovery horizon, limits and failure matrix
in a focused runtime guide. Public deployment remains blocked until this gate passes.
