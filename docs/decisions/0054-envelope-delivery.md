# 0054: Envelope delivery and mailboxes

Status: accepted.
Date: 2026-10-10.

## Context

`share` returned one sealed group-secret envelope per recipient and the
application carried it. The shopping example wrote envelopes to a replicated
`keys` table that every member read, so every member saw who received which
list and when, and after a revocation the application had to share the new
epoch itself. [Phase 12](../features_07/12-direct-key-delivery.md) delivers
envelopes to their recipient alone, inside the SDK; its "Settled" answers are
the product decisions this record implements.

## Decision

**Wrapping.** `rocky_credentials::SealedDelivery` (version 1) seals a payload
to one recipient's Ed25519 node key, converted to X25519, with a fresh
ephemeral key and ChaCha20-Poly1305 under
`rocky.envelope-delivery.key.v1`. Only the version, the recipient's node key
and the ephemeral key are in the clear. The SDK's payload (contents version 1)
holds the reason, `share` or `rotation`, the group name, its canonical slot
values, an optional sender credential chain and the inner [group secret envelope](0050-concurrent-key-epochs.md),
which keeps its signature, recipient and epoch checks. An issuer signs with
the publisher key; a member signs with its node key and includes its chain
inside the outer sealing. The recipient verifies whole-group read on that
chain against the current revocations before trusting the signer. A payload is at most 112 KiB, so a delivery fits one
session message. A delivery's digest is
`rocky.envelope-delivery.digest.v1` over its encoding: the same bytes reach
the recipient directly and through every mailbox.

**Receipt.** `DeliveryReceipt` (version 1) is the recipient's node-key
signature under `rocky.envelope-receipt.v1` over the delivery's digest.
Anyone holding the delivery verifies it without the payload.

**Outbox.** `identity.json` version 13 adds one field, `deliveries`: each
envelope this device sends, by digest, with its recipient, group address,
epoch, the sealed delivery, the mailbox targets reserved before any deposit
call and the mailboxes that acknowledged storing it. A lost deposit answer
reuses its reserved target, so crashes cannot cause a fourth mailbox deposit. `share` saves
its deliveries before it returns; a rotation saves its deliveries in the same
write that clears its intent, once the registry tombstone committed. Its
recipients come from the registry: every unrevoked device whose record reads
every member of the group partition in whole, by profile or by a recorded
grant. A delivered entry drops the delivery and stays for `status`; 16,384
entries are kept, delivered ones replaced first.

**Session.** `rocky_core::net::NATIVE_SESSION_VERSION` becomes 11 with one
new family, `Delivery`. After admission the dialer runs one round before it
moves rows, so a list a delivery opens syncs in the same exchange:

1. `Hand`: deliveries addressed to the acceptor. It verifies and installs
   each and answers `Receipts`; the dialer marks those delivered.
2. `Deposit`: deliveries for other recipients, only to a mailbox. The
   mailbox answers `Deposited`, a refusal or none per deposit; the dialer
   records the mailboxes that stored it.
3. `Collect`, repeated while more is held: the acceptor answers `Collected`
   with its own deliveries for the dialer, its deposits for the dialer and
   the receipts of the dialer's deposits. The dialer installs, then sends
   `Taken` with its receipts and the digests of the receipts it took; the
   acceptor marks its own delivered, keeps receipts in place of deposits and
   forgets the taken receipts.

Pages hold at most 96 KiB of deliveries and 256 items; a round runs at most
64 collect pages. The owner of the state implements the `Deliveries` port of
`rocky-secure`, which carries opaque deliveries only.

**Mailbox.** A mailbox is any admitted peer whose chain carries a custody
grant, including partition grants, and every issuer; it is
independent of the partition. A sender deposits one envelope with at most
three mailboxes, one hop. A mailbox stores deposits by digest in
`mailbox.json` (version 1) with the depositor, the recipient's node key and
the arrival time, saved after every change. It serves a deposit only to the
session authenticated as its recipient, keeps that recipient's verified
receipt in place of the deposit and hands it to the depositor, then forgets
it. Bounds: 256 deposits per recipient and 4,096 in all, receipts included,
each with an envelope of at most 4 KiB plus bounded wrapping
(1 KiB for partition metadata and 32 KiB for sender credentials, at most
37 KiB of sealed payload); a deposit is gone 30 days after it arrived. At either
bound it refuses `mailbox_full`, which is retryable: the sender keeps the
envelope.

**Recipient.** Opening a delivery requires the recipient's node key. The
recipient checks that the named partition's address is the envelope's group,
then installs as `install` did: whole-partition read on every member, the
authorized signer's signature and the epoch identity. An epoch it holds already installs
nothing and is acknowledged. A shared partition it did not hold starts from a
peer's checkpoint; a rotation adds the epoch. If the rotation arrives
before the first share, it also saves the initial checkpoint intent, so
arrival order cannot skip bootstrap. Existing released holdings stay released. A refused envelope gets no
receipt.

**Revocation.** A sender drops every envelope of a recipient its set revokes.
A mailbox removes a revoked recipient's deposits when it learns the statement
and serves none; the handshake already refuses the recipient.

**Surface.** `share` returns its epoch and recipients, no envelope.
`partition_install` with a supplied envelope is gone. `status` reports, to
the sharer alone, each recipient's envelopes as `pending`, `deposited` or
`delivered`. Collection reader keys stay with renewal.

## Consequences

- No replicated row says who received which partition. A mailbox learns that
  a recipient was sent something, when and how large, and no partition.
- A member without custody that shares nothing stores no other device's
  node key for delivery.
- Several issuers sharing one partition each deliver their own envelope; the
  recipient installs the epoch once.
- An envelope or wrapping beyond its bound is refused before an outbox
  save; no delivery is reported successful for it.
- A recipient whose grant arrives after its envelope acknowledges nothing
  until it can install it; its sender keeps the envelope meanwhile.
- `rocky/native-session/10`, `identity.json` version 12 and every earlier
  format are refused; existing installations reinstall.
