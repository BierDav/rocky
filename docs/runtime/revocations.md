# Revocations

A revocation is one signed statement; what a device enforces is the set of
statements it verified. Sets merge by union, so issuers revoke concurrently
and every device ends with every statement. `rocky-credentials` defines the
statement and the set, `rocky-secure` admits by them and `rocky-sdk` signs,
carries and enforces them. The rationale is
[decision 0049](../decisions/0049-mergeable-revocation-set.md); key rotation
is in [credentials](credentials.md#key-epochs-and-rotation).

## Statements

`Revocation` names the revoked node, user and pseudonym keys and capability
IDs, the issuer's node key, a time and the application it belongs to. The
publisher key signs it and the issuer's node key co-signs it:
`SignedRevocation::sign(statement, publisher, issuer)`, or `Authority::revoke`.
`verify(publisher, app)` checks both signatures, the application and the size
(2,048 bytes encoded) and returns the statement's ID, the hash of the
statement. The co-signature attributes; it authorizes nothing. A statement
counts also when its issuer is itself revoked: two issuers revoking each other
are both revoked.

## Sets and digests

`RevocationSet` holds statements in ID order.

- `merge(other, publisher, app, time)` adds every live statement it lacks and
  drops its own expired ones. One statement that fails verification refuses
  the whole merge and changes nothing; order and repetition never refuse.
- `digest(time)` identifies the live statements. Devices holding the same live
  statements share it, whatever order they learned them in.
- `live(time)` counts the live statements; `revokes(key)`, `ends(link)` and
  `revoking(chain)` answer for one key, one credential link or one chain.

A chain is ended when some statement names a link's subject key or a link's
capability ID. A statement's time never enters admission.

## Expiry and tombstones

A credential link is valid for at most 366 days plus one day of skew;
verification and issuance refuse a longer one. A statement leaves the set on
the first day, counted from the Unix epoch, on which its time plus 366 days
plus skew has passed: every chain it could end has expired by then. Merges
drop it and the digest leaves it out, by the same day-rounded cutoff on every
device.

What keeps a revoked user out afterwards is the issuers' tombstone: an issuer
records every node and user key of every statement it learns in
`authority.tombstones` and refuses their renewal for good. An issuer cut off
from every other issuer for longer than a chain's lifetime may never learn a
statement and readmit its user.

## Admission

`EncryptedReplica` holds a set, given in `SecureConfig.revocations` and
persisted in `secure_metadata`; reopening keeps the union of both.
`learn_revocations` merges another set; when its set changed it ends every
session and cursor and re-admits pending frames. Chains and frames verify under
the held set:

- every frame of a revoked author is refused, earlier ones included; rows of
  that author a device did not hold arrive by checkpoint;
- frames of every other author are admitted whenever they were signed;
- confirmed command results and historical checkpoints verify with no
  revocation at the time they were issued, since a receipt anchors them.

A native session's hello carries the sender's digest. Once both sides
authenticated, each refuses a peer whose digest differs with
`network::REVOCATIONS_DIFFER` ("revocation set differs"); neither serves the
other. [Phase 9](../features_07/09-roster-free-admission.md) carries the
statements in the session instead.

## In the SDK

`revoke(node)` on an issuer builds the statement and each scope's target key
epoch from the device's [registry](issuer-registry.md) record and saves them as
the intent, then makes the keys, signs the statement, renews its own chain,
clears the intent and writes the record's tombstone
([key rotation](credentials.md#revocation-in-the-sdk)). It refuses with
`revocation_set_full` at 1,024 live statements, before anything is saved.

Renewal answers, enrollments, supplied credentials and the answer to
`rocky.sdk.policy.v1` carry the issuer's whole set; the receiver merges it.
A sync whose peer refuses its digest (`revocations_differ`) renews once from
that peer, or from its issuers, and retries; a renewal that brought new
statements starts a checkpoint bootstrap for the rows of revoked authors. A
device refused renewal fetches the set with `rocky.sdk.policy.v1` and keeps
the statement that revokes it in `revoked.json`. Status, readiness and the
`renewed` activity report `revocations`: the digest and the live statement
count; Kotlin's `SessionStatus` and `Readiness` carry them as
`RevocationStatus`.

## Limits

- Until [phase 8](../features_07/08-concurrent-rotation.md), two issuers
  rotating one scope at once end in "key differs from retained crypto state"
  where the second key arrives. Concurrent revocations of devices reading
  disjoint scopes merge.
- An issuer rotates only the group partitions whose secret it holds.
- Revoking a device another issuer enrolled rotates every collection
  ([phase 7](../features_07/07-registry-revocation.md)).
- Until phase 9 a member learns statements only from an issuer's renewal; two
  members with different sets refuse each other until one renews.
- Devices whose clocks straddle a day boundary near an expiry hold different
  digests for up to a day.
- Two partitioned issuers near the bound may together hold more than 1,024
  live statements; a set is bounded to 2 MiB.

## Formats

| Record | Content |
| --- | --- |
| `Revocation` | `app`, `issuer`, `time`, `nodes`, `users`, `capabilities`, `pseudonyms`; ID `rocky.revocation.id.v1` |
| `SignedRevocation` | Publisher signature `rocky.revocation.v1`, issuer signature `rocky.revocation-issuer.v1`; at most 2,048 bytes |
| `RevocationSet` | The signed statements in ID order, canonical CBOR, at most 2 MiB; digest `rocky.revocation-set.v1` over the live IDs |
| Native session | `rocky.native-session.v8`, hello with `revocations` |
| Checkpoint statement | Format 7, `revocations` digest of its signer |
| Horizon statement | Format 3, `revocations` digest of its declaring issuer |
| Encrypted replica | `rocky.encrypted-replica.v9`; `secure_metadata.revocations` |

## Qualification

- `cargo test -p rocky-credentials --lib revocation`: every interleaving of
  merges gives one digest, expired statements leave it, a bad statement
  changes nothing, mutual revocations both count.
- `cargo test -p rocky-secure --lib tests::revocations`: a learned revocation
  refuses the revoked author and admits other authors' earlier frames; a hello
  with another digest is refused by name until both hold the set.
- `cargo test -p rocky-sdk --lib enrollment::tests`: the fault sweep over
  `revoke`, the 1,025th live statement and tombstones at every issuer.
- `cargo test -p rocky-sdk --test revocations`: two partitioned issuers each
  revoke a device, joined in both orders, on the in-process hub.
