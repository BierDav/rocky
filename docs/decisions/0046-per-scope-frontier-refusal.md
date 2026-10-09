# 0046: Per-scope frontier refusal

Status: accepted.
Date: 2026-10-09.

## Context

A grant on a partition is a selector, resolved against the partition's tuple.
A replica that has not learned a partition, such as a list another issuer
created before the two devices met, cannot resolve any grant for it. The
[frontier exchange](0044-group-frontier-exchange.md) required read or custody
on every member, so such a member refused the whole request and ended the
session in both directions, before the envelope that would teach the tuple
could be delivered. Admitting unresolvable members instead would let any peer
with a valid chain push to, and read ciphertext and log metadata from, a
replica that relays a partition it has not learned.

## Decision

The current native session is `rocky.native-session.v7`, with transport name
`rocky/native-session/7`. Its handshake signature domain refuses all other
versions. There is no earlier reader, negotiation or fallback.

`Frontiers` answers each member of a `FrontiersQuery`, in request order, with
either its state or `Refused(scope)`. After verifying the chain once, the
server refuses a member unless the peer's grants resolve to whole-scope read or
custody on it; an unlearned partition resolves none. It records acknowledgments
and exposes metadata only for the other members. Malformed batches, invalid
chains and invalid custody offers still refuse the whole request.

Before sending, the client filters each batch to the scopes that the server's
verified credentials let it hold whole, so a frontier is never disclosed to a
peer outside its scope. A chain of a generation this replica cannot verify is
not filtered, preserving the upgrade signal. `changed_scopes` returns the
changed scopes and the refused ones from both sides. Refused scopes are not
pulled, pushed or horizon-acknowledged in that session. Per-scope range,
push, checkpoint and horizon requests keep their strict authorization.

The SDK records a `scopes_refused` activity event with the peer and the
refused scopes when that set changes for a peer, including to empty.

## Consequences

Stored formats are unchanged; only native-session peers must use the current
wire version. A list created on another device reaches this one only after it
is shared and installed here; until then sync continues for every other scope.
A peer whose access to one scope was revoked keeps syncing the rest. A replica
never holds or serves ciphertext of a partition whose tuple it lacks, except as
a custody node that learned the tuple through a [custody offer](0045-custody-partition-offers.md).
