# 0072: Coordinated log and ordinary coordinated writes

Status: log implemented; application write integration specified for phase 9.
Date: 2026-10-14.

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

## Consequences

A permanently lost in-sync set needs explicit owner recovery (phase 10); a
minority never synthesizes a successful write. Trusted nodes must acknowledge
only durable, unapplied bundles. Node identities need durable sequence allocation,
and head reads distinguish assigned from committed positions.

Raft store/snapshots use only format 3, refusing other formats. The signed
bootstrap and request envelope keep their current shapes; log actions add new
operations. The current log machine fixes trusted-key, minute-rate and deadline
caps; changing those caps requires a new store format. Commands still exist until
phase 11; phase 8 does not implement application bundle staging, publication or
recovery cuts. See [the log guide](../runtime/coordinated-log.md) and
[phase 9](../features_09/09-coordinated-writes.md).
