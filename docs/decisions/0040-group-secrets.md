# 0040: Group secrets and derived table keys

Status: accepted; implemented by [phase 3](../features_06/03-group-secrets.md).
Date: 2026-10-09

## Context

[Decision 0037](0037-partition-groups.md) gives a group partition one address
and requires phase 3 to pin the secret, the key derivation and the envelope.
Each table partition previously had its own random key, envelope and rotation,
and revocation missed partitions granted after enrollment.

## Decision

**Secret.** One random 32-byte secret per group address and key epoch. The SDK
stores it in the installation's key provider as `group.<address hex>.<epoch>`.
Table keys are never stored; the runtime still holds one key per table scope.

**Derivation.** A table key is HKDF-SHA256 (RFC 5869, one output block) with
salt `rocky.group-table-key.v1`, the secret as input keying material and info
`group address (32) ‖ mesh (16) ‖ app (16) ‖ domain (16) ‖ table identity (32)
‖ epoch (u64 big-endian)`. The epoch is positive and at most `i64::MAX`. A
derived key reveals neither the secret nor a sibling table's key.

**Envelope.** `GroupSecretEnvelope` version 1 replaces the per-table partition
key envelope. Its canonical CBOR body holds the version, manifest, group
address, epoch, issuer, recipient, ephemeral X25519 key and sealed secret. The
wrapping key is the commitment `rocky.group-secret-envelope.key.v1` over the
X25519 shared secret and the context; ChaCha20-Poly1305 seals the secret with a
fresh nonce, so the sealed length is the secret length plus nonce and tag. The
issuer signs the body under `rocky.group-secret-envelope.v1`; the context is
`rocky.group-secret-envelope.context.v1`. Decoding is bounded to 4 KiB; any
other version or sealed length is refused.

**Identity.** SDK identity version 9 records group partitions (name, canonical
slot values, address and held epochs) and, on an issuer, each partition holder
(node, user and recorded grants). Version 8 held per-table partition keys and
is refused; there is no migration.

**Admission.** Installing a secret requires whole-partition read on every member
table. The envelope is opened and read verified before any address is
registered or secret stored; the epoch capacity is checked before the secret is
written.

**Rotation.** Revocation creates one new secret per affected group partition;
every member advances to the same epoch. Affected partitions are those whose
members the device's enrollment profile reads or whose members a recorded
partition grant selects, or every known partition when the issuer never
enrolled the device. The holder's user subject is revoked with the device.

## Consequences

One envelope per recipient and one rotation per group partition, independent
of table depth. A table that joins the group later derives its key from the
held secret. Reading is all-or-nothing per group partition, as 0037 accepts.
A grant delegated on a device that is not an issuer is not recorded by any
issuer; revoking that device rotates every known partition conservatively.
Another issuer learns a new epoch only from an envelope.
