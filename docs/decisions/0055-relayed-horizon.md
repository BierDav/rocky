# 0055: Relayed horizon evidence

Status: implemented in Phase 13.

## Contract

A declaring issuer selects devices from its replicated registry, including
whole-scope readers and custodians, and uses their latest recorded chain
expiry. An explicit participant list selects from those same records; neither
contacts nor direct sync are prerequisites. More than 64 participants returns
`horizon_participants_full`. Expired devices are immediately behind.

Horizon format 4 replaces format 3. Its sorted `policy` statement IDs bind the
existing revocation digest. Holders verify that digest and that every referenced
revocation is known at admission. An already verified statement remains valid
when its referenced revocations expire from the live set. Additional revocations
do not invalidate a pending cut:
a locally revoked participant no longer contributes a required wait. The
publisher signature, manifest, scope and dependency bindings remain mandatory.

Native session 12 replaces 11 and adds `HorizonRelay`, an exchange of one scope's
signed statement, up to 64 participant acknowledgments and an optional signed
finalized checkpoint. Both request and response are behind the holding proof.
An unproved scope gets the same refusal and no evidence as an unknown scope.
The session peer need not be the acknowledgment signer. The statement ID and
participant key, authenticated by the acknowledgment signature, authorize it.

Custody admits evidence without reproducing a row cut or acknowledging it.
Readers still reproduce the exact cut before signing. Statements and
acknowledgments survive restart. One statement per scope and its participant
set bound pending storage. A signed finalized checkpoint clears custody's
pending index and acknowledgments. One completed certificate per scope keeps
that checkpoint and its original, individually signed acknowledgments available
for relay, bounded by the same 64 participants. A participant may finalize
before its issuer fetches the acknowledgments; clearing pending storage must
not make that issuer wait for expiry. Certificates never acknowledge another
cut, and a successor's finalization replaces the previous certificate. Row
checkpoint import and pruning keep their existing checks. A reader whose rows or parent cut differ from finalized evidence enters
the existing rebootstrap guard before importing its checkpoint. No relay
acknowledges for another device.

## Concurrent declarations

At the same sequence and predecessor, the lower signed statement ID wins.
Replacement drops all acknowledgments of the higher ID and rebuilds the
pending dependency index in the same transaction. Devices acknowledge the
winner again; losing acknowledgments never enter its set.

If the higher ID already finalized locally, replacement installs the existing
rebootstrap guard. That holder cannot write or acknowledge until it imports
the lower winner's finalized checkpoint. A same-sequence lower winner is an
allowed replacement in checkpoint admission; a higher sibling cannot undo it.
This can require waiting for the guarded device's fixed deadline before the
winner finalizes. The 30-day bound and durable-copy gates are unchanged.

## Storage and qualification

Encrypted replica identity v12 replaces v11. A bounded per-scope table retains
signed finalized relay evidence alongside the existing statement and acknowledgment
tables. The met-peer credential cache remains because chain retention and the
bounded admission peer cache still name it; it no longer chooses participants.
No old reader, migration or version negotiation is added.

See [the horizon guide](../runtime/mergeable-horizon.md) and
[Phase 13 evidence](../features_07/13-relayed-horizon-evidence.md).
