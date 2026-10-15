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
  that author a device did not hold arrive by checkpoint. A replica never
  sends such a frame, so one that relayed them still reconciles with peers;
- frames of every other author are admitted whenever they were signed;
- confirmed command results and historical checkpoints verify with no
  revocation at the time they were issued, since a receipt anchors them.

Every [live link](live-links.md) handshake ends with the
[revocation exchange](../decisions/0051-roster-free-admission.md): both sides
compare digests and, when they differ, send each other the statements the
other lacks, then check the merged set. A peer it revokes is refused as `the
peer is revoked`; a side it revokes ends the session with `network::Revoked`,
holding the statement. A replica never merges the statement that revokes its
own chain: its owner records it instead.

## In the SDK

`revoke(node)` on an issuer builds the statement and each scope's target key
epoch from the device's [registry](issuer-registry.md) record and saves them as
the intent, then makes the keys, signs the statement, renews its own chain,
commits the record's tombstone and then discards the redo intent
([key rotation](credentials.md#revocation-in-the-sdk)). It refuses with
`revocation_set_full` at 1,024 live statements, before anything is saved.

Renewal answers, enrollments, supplied credentials and the answer to
`rocky.sdk.policy.v1` carry the issuer's whole set; the receiver merges it.
Sessions carry the set too: a device learns a statement from any admitted peer
it meets. Rotated keys still arrive by renewal, so a sync whose session changed
the set renews once from that peer, or from its issuers, and retries; a pass
renews first when a served session brought statements. New statements start a
checkpoint bootstrap for the rows of revoked authors. A device refused renewal
fetches the set with `rocky.sdk.policy.v1`, and one a session revoked holds the
statement from the handshake; either keeps it in `revoked.json` and reports
a typed revocation failure. `DatabaseStatus.authority.verifiedRevocation`
retains the evidence identity and signed statement. Its `revocations` and the
`renewed` activity retain the held set digest and live statement count.
Revocation is an authority fact, independent of links or networking controls.

## Limits

- Until [phase 8](../features_07/08-concurrent-rotation.md), two issuers
  rotating one scope at once end in "key differs from retained crypto state"
  where the second key arrives. Concurrent revocations of devices reading
  disjoint scopes merge.
- An issuer rotates only the group partitions whose secret it holds.
- Recorded access selects scopes precisely at any issuer. Unrecorded devices
  rotate all collection keys and known partition secrets held here; only
  their node is named. Non-issuer delegation provenance remains unrecorded
  ([phase 7](../features_07/07-registry-revocation.md)).
- A member learns statements from any peer, but the keys a revocation
  rotated only from an issuer's renewal; without one its rows wait for them.
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
| Live link handshake | `rocky.link.v5`; the revocation exchange of [0051](../decisions/0051-roster-free-admission.md) |
| Checkpoint statement | Format 11, `revocations` digest of its signer |
| Horizon statement | Format 4, `revocations` digest of its declaring issuer |
| Encrypted replica | `rocky.encrypted-replica.v21`; `secure_metadata.revocations` |

## Qualification

- `cargo test -p rocky-credentials --lib revocation`: every interleaving of
  merges gives one digest, expired statements leave it, a bad statement
  changes nothing, mutual revocations both count.
- `cargo test -p rocky-secure --lib tests::revocations tests::admission`: a
  learned revocation refuses the revoked author and admits other authors'
  earlier frames; a handshake leaves both sides with the union; a revoked
  peer learns its statement, which its replica does not keep, and both sides
  end the session; a full set is paged within frames.
- `cargo test -p rocky-sdk --test admission`: a member learns a revocation
  from a peer that is no issuer and stops syncing.
- `cargo test -p rocky-sdk --lib enrollment::tests`: the fault sweep over
  `revoke`, the 1,025th live statement and tombstones at every issuer.
- `cargo test -p rocky-sdk --test revocations`: two partitioned issuers each
  revoke a device, joined in both orders, on the in-process hub.

- `cargo test -p rocky-sdk --test registry_revocation`: phase 7 on the hub,
  including another issuer revoking with the enroller stopped, partition-only
  selection, unrecorded-delegate full rotation, issuer registry-key rotation
  and two statements for one non-reader with one tombstone.
- `cargo test -p rocky-sdk --lib node::revocation_tests`: twelve interruptions,
  including each generated key, rollback before tombstone commit and a stop
  after commit; the real database open finishes with the same statement and
  keys and exactly one tombstone.
