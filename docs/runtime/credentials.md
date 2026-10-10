# Credentials, key providers and key rotation

`rocky-credentials` verifies and issues; `rocky-secure` enforces and keeps key
epochs; `rocky-sdk` offers [enrollment](sdk-enrollment.md) as optional plumbing.
Raw grants, delegation and pseudonyms are in
[credential primitives](credential-primitives.md); per-partition grants and key
envelopes in [partition credentials](partition-credentials.md). None of this
has had a cryptographic audit.

## The crate

`rocky-credentials` depends on `rocky-core` only. It opens no database and no
endpoint: everything it does is keys and bytes.

- Signed types: `SignedManifest`, `SignedCapability`, `CredentialChain`,
  `SignedRevocation` and the `RevocationSet` of them a device holds
  ([revocations](revocations.md)). [Author pseudonyms](credential-primitives.md#author-pseudonyms)
  and partition selectors use a versioned capability extension.
  `rocky-secure` re-exports these types.
- `Credentials { publisher, manifest, revocations, chain }::verify(device, time)`
  checks a whole supplied set and returns the principal, grants, revocation
  digest and expiry. Nothing needs to be contacted.
- `Authority { publisher, manifest }` issues user capabilities, delegable
  ones with `issue_delegable`, renews one with the same grants and depth and
  signs revocation statements co-signed by an issuer's node key. `device_chain` and
  `CredentialChain::complete` delegate to a node key, `CredentialChain::delegate`
  to another user or deployment.
- `Grant` is a [raw grant](credential-primitives.md#raw-grants); `Profile`
  produces raw grants and resolves them over named resources.
- `KeyProvider`, `FileKeyProvider` and `Vault` hold secrets.
- Enrollment and renewal messages without a transport: `Invitation`,
  `PairRequest`, `seal_enrollment`/`open_enrollment`, `RenewRequest` and
  `Renewal`.

Verification is necessary, never sufficient. A secure runtime verifies every
frame, session, sync request and transfer against the revocations it learned,
whichever issuer, transport or enrollment produced the credentials. Errors are
`Denied` (final for the same input), `Protocol` or `Provider`.

## Identities

Each identity is its own key or name. None implies another; only a chain rooted
in the publisher grants anything.

| Type | Key | Grants |
| --- | --- | --- |
| `Publisher` | Domain trust anchor | Signs manifest, revocations and user capabilities |
| `LoginPrincipal` | Subject of a chain's root | What its capability names |
| `SigningDevice` | Node key, the chain's leaf | Signs operations, session frames and renewal requests; owns author logs |
| `Pseudonym` | Separate signing key per device and table | Author number and table grants; issuer alone opens the sealed device binding |
| `TransportEndpoint` | Route key | Nothing: whoever answers must prove a signing device |
| `ReaderScope` | Encryption scope at a key epoch | Reveals every row sealed under that key |
| `CoordinatorControl` | Tenant control key | Registers and controls coordinator domains; no row, key or command right |
| `ExternalLogin` | A provider's name for a person | Nothing by itself |

A `LoginProvider` verifies an external assertion into an `ExternalLogin`. An
issuer may use that to decide whether to issue a capability to a user key; the
assertion carries no grant, scope key or executor eligibility. No provider
ships with Rocky.

## Grant profiles

Profiles are the optional starter layer: each is a function producing
[raw grants](credential-primitives.md#raw-grants), and nothing enforces them
beyond the grants they produce. `{"kind": "grants", "grants": [...]}` names
raw grants wherever a profile is accepted.

| Profile | Rights per named collection | Keys |
| --- | --- | --- |
| `custody` | `Custody` only | None |
| `reader` | `Read`, `Custody` | The named collections' |
| `writer` | `Read`, `Insert`, `Update`, `Delete`, `Custody` | The named collections' |
| `issuer` | Writer of every collection, plus the publisher key | All |

In the SDK, an `issuer` also writes and a `custody` peer holds the reserved
[issuer registry](issuer-registry.md); no other profile or raw grant names it.

Every grant selects whole rows. With [typed commands](typed-commands.md), a
writer also gets `Invoke` for the commands whose arguments are sealed for a
collection it writes, and an issuer `Execute` for every command; each is
paired with the command's manifest item. No profile holds `PublishSchema`;
`Execute` for a device that is not an issuer is a raw grant.
A profile naming no or an unknown collection is refused; there is no default.
In JSON a profile is `{"kind": "reader", "scopes": ["items"]}`, `"writer"` the
same, or `{"kind": "issuer"}`; unknown fields are refused.

## Key providers

`KeyProvider` has `load`, `store` and `delete` by name; names use only
`[a-z0-9._-]`. A provider never invents a secret: loading an absent name is
`None`. `Vault` scopes names to one installation as `rocky.<installation>.<role>`.
`get` fails for a missing secret, `create` refuses to overwrite one, and only
the invitation token is replaced. A lost or deleted key is therefore an error
at the next use, never a silently regenerated identity. The SDK's `SecretStore`
is this trait; the Kotlin `SecretStore` adapts a keychain to it.

`FileKeyProvider` is the fallback: one owner-only file (`0600`, directory
`0700`) per secret. Its threat model:

- Secrets are **plaintext at rest**. File permissions protect them from other
  users of the machine, not from the owner, an administrator, malware running
  as the owner, a backup or a copy of the disk.
- Use a platform keystore adapter where one exists. Android has
  `KeystoreSecretStore`, which seals each secret under a non-exportable
  Keystore key ([threat model](android.md#keystore-secret-store)); others
  implement the same trait.

Local projections are plaintext at rest as well; see
[permissions and sync](permissions-and-sync.md#transactions-and-recovery).

## Key epochs and rotation

An epoch is `{ generation, secret_hash }`, where the hash is domain-separated
from other commitments. `SecureConfig.keys` holds multiple identities per
scope. Writers choose the highest generation and, among its siblings, the
lowest hash. Every frame, bundle member, command part, effect event and package
names the complete identity that sealed it. See
[decision 0050](../decisions/0050-concurrent-key-epochs.md).

- `install_key_epoch` retains siblings. Installing a new winner ends sessions;
  later writes use it, while every earlier key still opens its own ciphertext.
  The same identity and key again does nothing. The same identity with other
  bytes, or key material held under another scope, is refused.
- `secure_keys` records the winning identity and table-key fingerprint;
  `secure_key_epochs` records retained identities. No secret is stored there.
- Reopen completes an interrupted installation from the configured identities;
  a rollback omitting the recorded winner is refused.
- A frame whose sibling the replica lacks waits as pending.
- History and signed bytes are never rewritten. Checkpoint offers are sealed
  under the held winner; a stale offer is replaced before serving.

### Retaining historical keys

`retire_key_epoch(scope, epoch)` drops an earlier epoch only when no retained
operation was sealed with it, that is after a checkpoint covered them and they
were pruned. The current epoch is evicted with `evict_key` instead. Recovery
packages carry the original frames of every epoch; a recipient needs each
epoch's key to read them all. The SDK retains epochs a device already holds and
never retires them automatically. Enrollment and renewal deliver every sibling of the current generation; [checkpoint bootstrap](sdk-bootstrapping.md) supplies earlier rows to a new
device or one that missed intervening epochs.

### Revocation in the SDK

An issuer's `revoke(node)` saves the intent in `identity.json` first: the
[revocation statement](revocations.md) naming the device's node and user keys,
its capability and its pseudonym keys, and the key epoch each collection and
group partition the device could read rotates to, the scope's held maximum
plus one. Both come from the device's [registry](issuer-registry.md) record;
a partition-only record rotates only the group partitions its grants select.
Without a record every held collection key and every known group partition
secret this issuer holds rotates, including an unrecorded delegate. Then it:

1. creates each of those key epochs;
2. signs the statement under the publisher key, co-signed by its own node key,
   and adds it to its set, which ends the device's chains and no other;
3. renews its own chain, saves the identity with its intent still present,
   and teaches the replica;
4. commits the tombstone into the device's [registry](issuer-registry.md)
   record, then clears and saves the intent before returning success.

An epoch records the actual IDs of the revocations its issuer knew when
creating it. After concurrent revocations meet, an issuer rotates only scopes
that a newly learned statement exposed: the winner lacks that statement's ID
and its device could read the scope. The registry supplies that selection;
an unrecorded device uses the broad fallback. Revoking a known non-reader
leaves the scope unchanged. Once issuers hold the same statements, concurrent
superseding epochs record the same IDs and the sibling order settles them
without another rotation.

The exposure window runs from the concurrent revocations until the superseding
generation reaches each writer. In that window a revoked device can open rows
under a sibling whose secret it already holds. It cannot open rows under the
superseding secret. One renewal delivers that generation to an offline writer.
Status lists siblings and identifies the winner writers use.

The tombstone transaction is the durable completion marker. `identity.json`
and SQLite are separate stores, so clearing the JSON redo intent is cleanup
after that commit, not an atomic write across both files. A stop before commit
leaves an intent to finish; a stop after commit leaves an intent that replays
the same statement and keys and keeps the existing tombstone. Database open
finishes this cleanup before admitting sessions or attaching its network.

Non-issuer delegation stores no recipient relationship. Revoking an
unrecorded delegate names only its node and rotates broadly; it cannot name
an unknown user or pseudonym, identify the delegator, or prevent a fresh node
being delegated to again. A non-issuer that learned the statement refuses
a new delegation to the named node or user before returning reader keys. Revoking the recorded delegator does not enumerate
its delegates, although shared chain ancestors can end their credentials.
[Durable delegation provenance](../ideas/delegation_provenance.md) is deferred.

An interruption anywhere completes at the next open with the same statement
ID and the same keys. Other devices learn the statement and receive the new
epochs, for the collections their grants read, with their next renewal from
an issuer that holds them; a sync refused for a differing revocation set
renews at once. The revoked device is refused renewal by every issuer, for
good when its node or recorded user is revoked, and never receives the new
key, so ciphertext written after the rotation stays sealed from it even when obtained elsewhere. `revoke` refuses
with `revocation_set_full` once 1,024 live statements are held, and "another
revocation is unfinished" while this issuer's previous one is; other issuers
revoke at the same time.

## Limits

- **Learned revocations.** A device enforces the statements it learned. An
  offline device that has not learned a revocation keeps accepting the revoked
  peer and keeps writing under the old key epoch, which the revoked device can
  read. Expiry bounds this window.
- **Frames of a revoked author.** Receivers verify a frame's credentials
  against the revocations they hold. Every frame of a revoked author is
  refused, earlier ones included; its rows a device did not yet hold arrive
  through [checkpoint bootstrap](strong-history.md). Frames of every other
  author are admitted whenever they were signed. The same check refuses
  frames whose author's chain has expired. Confirmed command results are the
  exception: a verified coordinator receipt anchors them, so they verify with
  no revocation at the time they were issued
  ([portable result evidence](encrypted-commands.md#portable-result-evidence)).
- **No retraction.** Data a device decrypted, and keys it held, stay with it.
- **Issuers.** Another issuer revokes a lost issuer by a statement, but the
  revoked one keeps the publisher key: excluding a stolen issuer needs a new
  domain ([issuer recovery](sdk-enrollment.md#issuer-recovery)). Two issuers
  revoking at once both count; two that revoke each other are both revoked.
- **Concurrent revocations** merge, within the [limits](revocations.md#limits)
  of one scope rotated twice at once and of issuers partitioned for longer
  than a chain's lifetime.
- **Whole-row scopes.** A key reveals every row of its scope. Partition
  selectors and recipient key envelopes define independent keys for declared
  partitions. A key still reveals its entire partition; subscriptions select
  scopes and never narrow what that key reveals.
- Coordinator control tokens: `SignedCoordinationGrant` lets a tenant key
  coordinate one application's domains, signed by its publisher;
  `SignedTenantAdmission` admits a tenant to a provider's service. Neither
  grants anything in the application. See [public coordination](public-coordination.md).
- Not implemented: login providers, keystores other than Android's, publisher key backup.

## Formats

| Record | Version | Content |
| --- | --- | --- |
| `identity.json` (`rocky_sdk` `VERSION`) | 13 | Profile, `revocations`, held key epochs per collection, group partitions with held secret epochs and the revocation IDs of each epoch, issuer `authority` (invitation and its claim, pending `rotation` with its statement and target epochs, `tombstones`), `issuers` by node key, author range and pseudonyms, and the envelopes this device sends in `deliveries`; device records are in the [registry](issuer-registry.md); 12 is refused |
| `mailbox.json` | 1 | A mailbox's deposits by digest: depositor, recipient node key, arrival, the sealed delivery or the recipient's receipt ([decision 0054](../decisions/0054-envelope-delivery.md)) |
| Sealed delivery, delivery receipt | 1 | Labels `rocky.envelope-delivery.*.v1` and `rocky.envelope-receipt.v1` ([decision 0054](../decisions/0054-envelope-delivery.md)) |
| Issuer registry rows | | One row per entry of a device's record ([decision 0048](../decisions/0048-replicated-issuer-registry.md)) |
| Invitation URI `rocky://join/` | 4 | Carries the grant profile and an explicit `via_custody` flag |
| Pair request | 2 | Labels `rocky.credentials.pair-*.v2`; the enrollment carries profile, key epochs and, for an issuer, the publisher key and an author range; optional `routes` are bound by the proof when present |
| Renewal | 3 | Request signed by the node (`rocky.credentials.renew.v2`), bound to an issuer audience and a one-time X25519 key; publisher-signed answer (`rocky.credentials.renewal.v3`) carrying the issuer's revocation set and sealing `{keys, issuers}` to that key |
| Contact URI | 2 | `issuer` and `enrollment_only` flags; `issuer` written only by issuers |
| Revocation statement and set | 1 | See [revocations](revocations.md#formats) |
| `revoked.json` | 2 | The statement that revoked this device |

`secure_keys` holds the current key fingerprint per scope and
`secure_key_epochs` every earlier one; the recovery inventory's `key_epochs`
names the latest per scope.

Installing key material already held under another scope or epoch is refused
with an explicit duplicate-material error, separately from missing read rights.

## Qualification

`cargo test -p rocky-credentials`, `cargo test -p rocky-secure --lib tests::rotation`
and `cargo test -p rocky-sdk --test credentials` cover supplied sets, attenuation,
profile grants, renewal refusing the revoked, a revoked reader failing on
post-rotation ciphertext, interrupted rotation and revocation completing once,
and epochs retiring only when unused. [Revocations](revocations.md#qualification)
lists the tests of the set, the frame rule and concurrent revocations.

## Design rationale

- `rocky-credentials` depends on `rocky-core` only, so credentials can be
  supplied from outside the SDK and verified without a database or endpoint.
- A delegated issuer is a second device holding the publisher key with its own
  user key, node key and author logs. Copying the creator's identity was
  rejected: two devices would sign one author log.
- Rotation never seals history again: it would rewrite signed bytes and could
  not retract what a revoked reader already holds. Ciphertext names its epoch.
- Revoking a subject key, not only a capability ID, ends capabilities another
  issuer signs later for that key.
- `authority.rotation` is saved before any key is made or anything is signed,
  so an interrupted revocation completes with the same keys and statement.
- An issuer never revokes itself, and revocation recovers from a lost issuer,
  not a stolen one: the publisher key cannot be taken back.
