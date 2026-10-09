# 0045: Custody partition offers

Status: accepted.
Date: 2026-10-09.

## Context

[Phase 10](../features_06/10-qualification.md) shares lists through a packaged,
configuration-only custody peer. A keyless custody node holds a partition scope
only after `partition_register` names its tuple, and a scope hash does not reveal
the tuple. The packaged peer has no control API, and each list is a new
partition created on a phone after the peer enrolled. A device that synced with
such a peer had its whole frontier batch refused, which surfaced as an
unexpected end of file.

## Decision

This record introduced native session v6;
[decision 0046](0046-per-scope-frontier-refusal.md) defines the current version.

`FrontiersQuery` carries `partitions`: `(scope, tuple)` pairs for members of the
batch. A client includes a pair only when the serving peer's verified
credentials grant custody without read on that concrete partition, so a tuple
is revealed only to a keyless custody node authorized for it.

The server validates the whole list before registering any pair: each scope is
in the batch, its tuple hashes to it, the server's own credentials grant custody
without read, and the sender's credentials grant read or custody on that
partition. Accepted pairs are registered in one transaction, under the existing
4,096-partition bound. A known partition is unchanged. Any other offer refuses
the request. Frontier exchange then authorizes each member, refusing only
those the sender may not hold ([decision 0046](0046-per-scope-frontier-refusal.md)).

## Consequences

The private replica format is unchanged: offers persist in the existing
`secure_partitions` table, so a restarted custody peer keeps them. Only peers
on the current session version interoperate. A custody node learns the
partition tuples of the lists it relays, as `partition_register` already
required. Readers and writers never register offered partitions; they still
install a group secret and subscribe or bootstrap. Horizon statements and
acknowledgments are not relayed by a keyless custody node.
