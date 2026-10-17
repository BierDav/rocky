# 0072: Coordinated log and ordinary coordinated writes

Status: implemented; the log by phase 8, coordinated writes by phase 9.
Date: 2026-10-14.
Revised 2026-10-18 for scoped custody and sealed-frontier readiness; the repair's
[qualification and independent acceptance](../features_09/17-schema-changes-through-seals-evidence.md#qualification)
are recorded separately.

## Context

A command receipt orders one executor's results. Applications also need ordinary
row bundles ordered across trusted nodes, without placing payloads on a coordinator
or allowing a disconnected node to continue a competing history.

## Decision

The coordinator owns one metadata log per domain. Its control certificate's
executor keys are the trusted node keys during the transition away from commands.
A node joins with the current commit position and running hash; membership cannot
change during a pending entry. The owner sets a minimum, default one, before the
first join. Below it no append is admitted.

An append compares the exact head and membership epoch, assigns the next position,
and binds a hash, proposer and capped deadline. Every in-sync node, including the
proposer, must sign a storage acknowledgment for that exact pending entry before
it commits. Before the deadline missing acknowledgments leave it pending. At the
deadline missing nodes leave: the remaining set commits if it meets the minimum,
otherwise the entry aborts. An abort preserves the commit index and running hash;
its epoch fences the discarded attempt before that position can be reused.

Join, append, commit, abort, policy and membership are Raft-replicated. Storage
acknowledgments stay in leader memory and are collected again after election. The
resolving Raft entry includes signed acknowledgment proofs, checked on every voter;
committed member positions persist, while partial acknowledgments do not. A lazy
leader timer resolves pending deadlines and recovers timers on election. There
are no coordinated-log heartbeats or per-node idle timers.

Join/append identities are monotone per-node/domain sequences with a digest and
one retained first outcome. Earlier identities are retired; changed input at the
last identity conflicts. The running hash covers committed entry hashes in order.
Only the last 1,024 entry hashes remain, encoded as one bounded byte string. Tenant
and mesh quotas reserve the full bounded log capacity at the first admitted node
request; later commits cannot fail for incremental window growth.

Phase 9's contract is store, acknowledge, commit, then apply and publish ordinary
bundles. Nodes catch up from peers before joining; members receive only committed
entries. Neither this metadata log nor its trusted-node designation grants row
access. The coordinator cannot verify a storage promise against application data.

A trusted node may coordinate its authorized scopes without holding every scope
in the domain. Entry storage refuses before durable storage or acknowledgment
unless every frame's scope has a whole-row Read or Custody grant. Neither replica
role nor possession of a key substitutes for that grant. Readiness at an inherited
seal may use verified author-signed header placements for scopes outside custody;
held scopes still require their payloads and applied state. An unproved partition
returns `custody_required` without disclosing its tuple. See
[finalization](../runtime/lazy-compaction.md#finalizing).

A proposer stages its bundle in one flushed local transaction on caught-up state,
unsent, and records the append before calling the coordinator. The entry hash
commits to the log, the position and the bundle's frame ids; frames carry the
position in their bundle membership, not the hash, which would have to cover
itself. Peers store an entry durably and unapplied, then acknowledge it themselves.
Committed entries apply in log order. A failure before the coordinator assigned the
entry, or an abort, voids the proposer's frames with phase 4's void records and
rebuilds their scopes; unsent bundles that saw them become held changes. Only an
outcome the coordinator may still decide (`Undecided`) keeps local effects, unsent.
Catch-up carries positions, hashes and frame ids verified against the head; the
frames, committed and ordinary, travel by range sync. Runtimes reach the log through
the metadata-only `rocky_core::DomainLog` port.

## Consequences

A permanently lost in-sync set needs explicit owner recovery (phase 10); a
minority never synthesizes a successful write. Recovery now uses
[phase 19's owner seal](../features_09/19-recovery-as-a-bookmark-move.md).
Trusted nodes must acknowledge
only durable, unapplied bundles. Node identities need durable sequence allocation,
and head reads distinguish assigned from committed positions.

Raft store/snapshots use only format 4, refusing other formats. Phase 10
adds the genesis anchor to the log head for fresh-node reconstruction. The signed
bootstrap and request envelope keep their current shapes; log actions add new
operations. The current encrypted file identity is
`rocky.encrypted-replica.v32` and the live-link handshake is `rocky.link.v14`,
including header placement requests; other versions are refused. The current log machine
fixes trusted-key, minute-rate and deadline caps; changing those caps requires a new
store format. Phase 11 removed commands; phase 19 replaces phase 10's recovery
cuts. See [the log guide](../runtime/coordinated-log.md),
[coordinated writes](../runtime/coordinated-writes.md) and
[phase 9](../features_09/09-coordinated-writes.md).
