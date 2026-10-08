# Stored cut rows for strong requests

Status: deferred. The bounded release it would replace is described in
[retention holds](../runtime/retention-holds.md#an-unresolved-strong-request).

## Trigger

Build this when a domain must keep strong requests executable through a long
coordinator outage or a slow executor, and releasing them is not acceptable.
Releasing is the current answer, and it leaves the request unexecutable on this
replica once a checkpoint passes its cut.

## Current cost

A strong request is sealed at a cut: an applied frontier and the digest of the
visible state there. Executing it, and verifying its result on any replica,
rebuilds that state from the checkpoint copy plus the retained operations up to
the cut. So an unresolved request holds checkpoints, which hold pruning, which
ends in `Error::Retention` at the byte bound.

The `request_hold` policy and `release_command` bound this, but a released
request can no longer be executed or verified here. A request with an unknown
outcome cannot be released at all.

## Sketch

Store the sealed state when the request is sealed, in the same transaction:

1. `seal` already produces the state bytes whose digest the request carries.
   Keep those bytes in a strong-ledger table keyed by request ID.
2. `replay_cut` uses the stored bytes when the cut is behind the checkpoint
   copy, after checking their digest against the request.
3. A replica that receives a request it did not seal stores nothing; it falls
   back to replay, as now. Optionally, a peer may send the bytes with the
   request, verified by digest.
4. The bytes are deleted with the request when its applied result is pruned.

Checkpoints then never wait for a request that this replica sealed, and the
`UnresolvedRequest` hold applies only to received requests.

## Costs and open questions

- The sealed state is the complete visible state of the cut. It can be as large
  as the projection and counts towards the strong ledger's byte bound, so a
  large domain may trade one bound for another. Measure the state size of real
  applications first.
- `execute_after` needs the reducer state, not only the sealed bytes. The
  handler contract must say whether the state can be rebuilt from its sealed
  form, or a second encoding must be stored.
- Verification by other replicas still replays; the stored bytes help only the
  sealing replica unless they are sent along.

## Must not break

- A result must still be checked against an exact replay or an equally strong
  digest check of the sealed state.
- Sealing must stay one local transaction with no coordinator call.
- The strong ledger must stay within its byte bound, with backpressure, never
  silent loss.
