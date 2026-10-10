# 0050: Concurrent key epochs

Status: accepted, 2026-10-10. Implements
[phase 8](../features_07/08-concurrent-rotation.md).

## Identity and writer order

`rocky_core::KeyEpoch` is `{ generation, secret_hash }`. The hash is
`commitment("rocky.key-epoch.v1", secret)` over the 32-byte collection key or
32-byte group secret. Generations remain positive integers bounded by SQLite's
signed integer range. The highest generation wins; within it the lowest hash
wins. `Ord` expresses this order, so every consumer selects `max`.

A group uses the same identity on every member table. Its table keys still use
0040's HKDF with the numeric generation, domain, group address and table.
Hashing a derived table key would incorrectly give the tables different
identities. Native callers use `KeyMaterial::new` for a collection key and
`KeyMaterial::at` for a derived key with its group identity.

Installation retains siblings and verifies the fingerprint of the actual table
key for each complete identity. Reusing an identity with different bytes or
held material from another scope fails. Missing siblings leave frames pending.
The 256-epoch limit counts all retained identities, including siblings.
Retirement requires the corresponding operations to have been pruned. The SDK
never retires automatically and history keeps its original signed bytes.

## Exposure and delivery

Every SDK epoch records the sorted, unique IDs of the signed revocation
statements known when it was created. They are immutable metadata authenticated
by the enrollment/renewal sealing or group envelope, not a set digest. An
issuer supersedes the winner only for a missing statement whose device could
read that scope, using phase 7's registry profile and partition selectors.
A tombstone without a device or grant record still uses the unrecorded-device
fallback. Non-readers cause no rotation.

The saved intent freezes its target generations and statement IDs. Its redo
secret slot is independent of the eventual hash, so a crash after writing the
secret uses the same secret on retry. The canonical slot includes the hash.
The group record is saved before installing its table keys; reopen installs
all members before admitting sessions. This also resumes received siblings.

After statements stop arriving, a superseding epoch records the merged IDs.
Concurrent superseding epochs with those same IDs are siblings; exchanging
them changes only the winner and does not trigger another generation. The
exposure window lasts until that generation reaches each writer. A device
holding a sibling of an earlier generation can still open its ciphertext.

Ordinary enrollment, renewal and `Supplied` deliver every sibling of the highest
held generation. The SDK's authenticated epoch exchange uses the existing
renewal transport and sealing but preserves the requester's capability. It
exchanges signed revocations and retained keys with reached issuers, retrying
the frame session after a changed catalog,
including earlier siblings needed for issuers' retained history. Members receive
only the current generation. Delivery verifies the
requester's chain against the merged revocations and registry tombstones.
This is the existing issuer route, not phase 12's direct-delivery mechanism.

## Current encodings

No legacy readers or migrations are retained. Earlier files reinstall.

| Location | Current representation |
| --- | --- |
| Frame extensions, including bundle members, command parts and effect events | Row format 7; `generation` holds the full identity in the signed header and encryption AAD |
| Bundle plaintext | Version 4; carried move keys name complete identities |
| Encrypted packages and carried move keys | Full identity in the signed/encrypted package metadata; package commitment domain `rocky.encrypted-package.v2` |
| Checkpoint statement `key_epoch` | Checkpoint format 8; full identity; an offer under a non-winning identity is replaced before serving |
| Recovery inventory | Format 4; per-scope sets of every required identity |
| Encrypted SQLite file | `rocky.encrypted-replica.v10` |
| `secure_keys`, `secure_key_epochs` | Encoded identity BLOB in `generation`, actual table-key fingerprint in `key_hash` |
| Group secret envelope | Version 2; full identity and recorded statement IDs authenticated in context and signed body; maximum 128 KiB for up to 1,024 IDs |
| Enrollment, renewal and `Supplied` key lists | Per collection, a list of `(identity, secret)`; group deliveries contain an envelope for every current sibling |
| `identity.json` | Version 12; identity lists and one `epoch_statements` map keyed by canonical secret role |
| Collection secret role | `reader.<collection>.<generation>.<hash-hex>` |
| Group secret role | `group.<address-hex>.<generation>.<hash-hex>` |

The file provider permits names up to 255 bytes to hold these qualified names.
Status lists complete identities and marks the winner per collection and per
group/member table. Hashes reveal identity, not the secret; trusted issuers can
choose secrets and therefore influence the tie-break.

The Kotlin `KeyEpoch` mirrors the writer order and JSON identity. Partition
status exposes `current`; the shopping example stores the complete identity as
text when publishing its existing key-delivery rows.
