# Final seals

A seal of a tip without a [coordinated log](coordinated-log.md) can still
lose to a concurrent seal that arrives late
([races](tips-and-bookmarks.md)). A seal its tip's log committed as the
terminal entry cannot: the coordinator commits one terminal per log. A
*final* seal is one every member can verify was that terminal, without
asking the coordinator. [Lazy compaction](lazy-compaction.md)
compacts a final seal at once; other seals only within a device's count of
seals left uncompacted.

## How a seal becomes final

1. A trusted node ends the tip's log with the seal's digest
   ([seals and moves](coordinated-writes.md#seals-and-moves)).
2. Each trusted node that follows the line reads the ended head and signs a
   terminal acknowledgment: the log `(scope, tip)`, the terminal's position
   and running hash, the seal digest and its continuation. It submits it to
   the coordinator, which keeps one per node with the terminal, beside the
   head. The coordinator checks the fields and the signer's key against the
   request; it never verifies the chain.
3. Any trusted node whose head holds enough acknowledgments publishes a
   finality record: the seal digest and the acknowledgments with their
   signers' chains. It needs no signature of its own and syncs to every
   device with the graph.

`EncryptedReplica::finalize_seals(scope)` does steps 2 and 3 for the
followed tip and the eight tips its line came through, as many ended logs as
the coordinator keeps. The SDK calls it after publishing tip statements,
after deciding coordinated entries and at the start of every sync pass; a
failure is the `finality_failed` [activity](activity.md) and retried by the
next call.

## What a member verifies

A record is admitted when it names a seal of the graph, every
acknowledgment names that seal's tip and continuation and the same position
and running hash, every signer is a distinct node whose chain is exactly an
owner-direct trusted grant for the domain and its node certificate, valid
and unrevoked at admission, and their number reaches the line's minimum:

- a tip a trusted identity opened: the opening's `minimum_in_sync`;
- the genesis line: the largest minimum the owner signed in a genesis line
  statement (`tips(action = "genesis_minimum")`,
  `Database::set_genesis_minimum`). Without one, no seal of the genesis line
  is final.

Resolution of a tip's seals becomes: a final seal, then the owner's, then a
trusted identity's, then an issuer's, then the lower digest. An owner seal
that arrives after another seal ended the log was never committed: the
owner's own publication fails `sealed`, and a received one stays in the
graph as a loser. To leave a final seal's line, the owner moves the
bookmark. Two final seals of one tip need trusted nodes to sign falsely; the
owner answers by revoking them. Revocation removes their acknowledgments
from the count, and a record left below its minimum no longer makes its
seal final; every device resolves again.

Inspection reports `final` for each seal and the genesis line's minimum.

## Formats

| Boundary | Current |
| --- | --- |
| Terminal acknowledgment | Format 1, signed `rocky.terminal-ack.v1` by the node key |
| Finality record | Format 1, at most 32 acknowledgments ordered by signer |
| Genesis line statement | Format 1, owner-signed `rocky.genesis-line.v1` |
| Coordinator store | Format 9: ended heads keep their acknowledgments |
| Live link / replica file | `rocky.link.v14` / `rocky.encrypted-replica.v32` |

Recovery seals also name the retained and replaced log points; their finality
checks the exact terminal position and running hash against the replaced point.
A recovery waits outside the graph until its proof verifies; see
[stuck recovery](stuck-recovery.md). Frames and existence records do not change.

## Design rationale

- Acknowledgments by distinct trusted nodes, not a coordinator signature:
  the coordinator stays an orderer that no member must trust.
- Any trusted node may acknowledge the committed terminal, not only the
  in-sync set at the terminal: a log no node joined still has one terminal,
  and safety rests on the minimum of distinct trusted signers.
