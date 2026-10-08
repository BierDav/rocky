# MVP strong-command contract

Status: implemented contract for phase 4; see [runtime APIs and recovery](../runtime/strong-publication.md). Read the
[operation contract](operation-contract.md) first. This is trusted in-memory
coordination, not a signed receipt protocol or durable Raft service.

## Application command and sealed input

`PublishListSnapshot` creates an immutable publication of visible todos at an
explicit applied frontier. It leaves the editable list untouched. One confirmed
publication sequence per domain exercises ordering without restricting offline
todo edits. A snapshot does not claim to include unknown offline work.

In one consistent local read, capture the frontier and visible rows. The request
binds mesh/app/domain, caller, stable request ID, method/version, schema/handler
generations, the frontier and its visible-state digest. Frontiers and rows use
the deterministic ordering in the operation contract.

A request ID is `(origin_node_id, request_sequence)` in a separate persisted
command counter namespace. Persist the request before dispatch; repeat delivery
uses the same bytes. Do not consume operation author sequences for commands.

The executor rebuilds precisely the cut from its checkpoint base and the
retained operations after it, and names that base in the package. Compare the
resulting visible-state digest to the request. Missing history stays pending; a
base past the cut, or none, stays pending without loading any operation; an
operation of the cut this replica rejected, or a mismatched digest, fails.
See [decision 0039](../decisions/0042-bounded-strong-cut.md).
Do not replace sealed input with the executor's newer live projection. The cut
must be dependency-closed and contain contiguous prefixes for each listed author.

## Canonical commitments

Use the operation contract's primitive binary encoding and SHA-256 over
domain-separated, versioned bytes. Do not hash unordered maps, SQL row iteration,
debug output or default JSON serialization. See the [implemented golden vectors](strong-encoding.md).

Use these fixed field orders, with u32 counts for collections and length-prefixed
embedded byte objects. Generations and method versions are u32; sequences/epochs
are u64; commitments are 32 raw bytes. IDs use the operation contract's types.

- State bytes: sorted rows of entity ID, title and completed, with row count.
- Request bytes: mesh, app, domain, caller, request ID, UTF-8 method name,
  method version, schema generation, handler generation, sorted frontier,
  visible-state digest. Request ID uses node bytes then command sequence.
- Package bytes: magic `rocky.package.v2`, length-prefixed request bytes,
  configuration, executor, epoch, expected sequence, expected head, schema
  generation, handler generation, publication sequence, frontier, cut base
  frontier, length-prefixed sorted-row state bytes.
- Confirmation: mesh, app, domain, configuration, executor, epoch, request ID,
  request hash, expected sequence, expected head, schema generation, handler
  generation, result commitment, new head.
- Receipt bytes: length-prefixed accepted confirmation bytes, followed by committed
  sequence (expected sequence + 1). They are attested by the trusted mock ledger,
  not by a signature added to an otherwise unverified object.

Frontier encoding is count followed by ordered node/sequence pairs. Row encoding
is count followed by ordered entity ID/title/boolean tuples. Repeat schema/handler
generations and frontier in the package only with values matching its request.
The method name is exactly `PublishListSnapshot` and method version is 1.

Use distinct ASCII prefixes `rocky.mvp.state.v1`, `rocky.mvp.request.v1`,
`rocky.mvp.result.v1` and `rocky.mvp.head.v1`, with a length-prefixed prefix before
each object's payload. A package's result commitment hashes its immutable bytes.
The successor head hashes previous head, successor sequence, request hash and
result commitment under the head prefix. It does not hash the live todo table.

The genesis head hashes mesh/app/domain/configuration under the separate prefix
`rocky.mvp.genesis.v1`, and is the head at sequence 0.
Configuration identifies the in-memory coordinator instance; never pretend a
new empty instance is the authority for a previously confirmed domain.

## Coordinator state and transitions

Per domain store owner/control fixture, coordinator configuration, executor,
monotonic epoch, active schema/handler generations, sequence/head and accepted
request records. Domain registration is explicit through the demo owner fixture;
an unknown domain must not be auto-created by a confirmation request.

Eligible executor IDs and takeover rights come from static fixtures. Operations
are atomic under the state-machine serialization boundary:

| Transition | Contract |
| --- | --- |
| Acquire | Assign the initial eligible executor at epoch 1; same-holder retry returns current assignment |
| Take over | Owner-authorized assignment using expected epoch; increment epoch even for an explicit same-holder fence |
| Read head | Return configuration, executor/epoch, generations, sequence and head |
| Confirm | Admit exactly one successor with matching domain/control, executor/epoch, generations and expected head |
| Lookup request | Return the established receipt, or absent if no confirmation exists |

Confirmation must include request hash and immutable result commitment but no
todo payload or application callback. The successor sequence must be current + 1;
derive/check the successor head using the commitment rule above.

Check authorization/scope first, then accepted-request deduplication before
current-epoch/head fencing. An identical previously accepted confirmation returns
its historical receipt even after takeover. A reused request ID with a different
request hash is rejected. Same request hash with a different result commitment
returns an explicit already-confirmed conflict and the established receipt;
the new package cannot be applied using that receipt. History is not re-executed.

For unaccepted requests, stale executor/epoch/head/generation fails without state
change. A rejected proposal may be reevaluated against the new head, preserving
its original request/input but storing a new immutable preparation attempt.

Keep accepted deduplication records for the bounded demo. No retention algorithm,
leases, leader election or consensus membership protocol is needed here.

## Receipt and result acceptance

The mock receipt binds coordinator configuration, domain, executor/epoch,
generations, request identity/hash, sequence, previous/new head and result
commitment. Validate it against the trusted coordinator's accepted record before
accepting incoming results. A struct labelled receipt is not sufficient evidence.

Check package hash, request binding, derived head, exact input/dependencies,
application policy and predecessor chain. Buffer packages missing required history
or earlier strong packages. An old-epoch receipt remains valid for a result
confirmed before takeover. An old executor's unconfirmed package remains invalid.

If the trusted coordinator is unreachable and a receipt has not already been
validated/persisted, keep receipt verification pending. Do not fabricate success.

## Durable app-node lifecycle

1. Acquire execution and catch up to the coordinator's confirmed head.
2. Validate caller/method and seal/replay the exact input cut outside a write txn.
3. Persist immutable prepared package and pending submission in a short local
   transaction. Keep it out of final publication queries and observer state.
4. Confirm outside the DB transaction. A timeout means unknown outcome, not failure.
5. In a second local transaction persist receipt, apply the publication, advance
   confirmed-head/revision state and enqueue the package/receipt for replication.
6. After commit, expose confirmed success and notify observers. A named peer's
   durable/apply acknowledgments are separate evidence.

After interruption, query request outcome before retrying or discarding. Keep
unresolved packages. If accepted, locate the exact committed package and resume
local application idempotently. If absent, reconcile epoch/head before retrying.
Each executor serializes domain commands; no coordinator wait holds a DB lock.

Replicate canonical package/receipt bytes and required dependency closure via an
explicit strong-result message kind; do not disguise it as a mergeable todo edit.

## Recovery limits

Test app-node restart while the same coordinator object remains alive. Loss of
the only confirmed package blocks strong progress; a head hash cannot recover it.
Never rewind the coordinator head to fit an older replica.

Full desktop/coordinator restart loses the in-memory ledger. If a persisted domain
has prior coordination state, expose `CoordinationStateLost` and block new strong
commands until a real recovery implementation exists. Mergeable edits may continue.
Do not reconstruct authority automatically from client projections or reset the
old domain to genesis. A fresh demo can use a new explicit data directory/domain.

## References

- [Coordinator rules, lifecycle and fencing](../proposals/architecture-proposal-v3.md#6-borrowing-company-coordination-without-uploading-code)
- [Missing confirmed payload and recovery](../proposals/architecture-proposal-v3.md#7-alices-deployment-and-recovery)
- [Strong-head versus local progress](../proposals/architecture-proposal-v3.md#13-sdks-sql-reads-and-observation)
