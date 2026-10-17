# Credentials, key providers and key rotation

`rocky-credentials` verifies and issues; `rocky-secure` enforces and keeps key
epochs; `rocky-sdk` offers [enrollment](sdk-enrollment.md) as optional plumbing.
Raw grants, delegation and author ranges are in
[credential primitives](credential-primitives.md); per-partition grants and key
envelopes in [partition credentials](partition-credentials.md). None of this
has had a cryptographic audit.

## The crate

`rocky-credentials` depends on `rocky-core` only. It opens no database and no
endpoint: everything it does is keys and bytes.

- Signed types: `SignedManifest`, `SignedSeal`, `SignedTipOpening`, `SignedCapability`,
  `CredentialChain`, `Endorsement`, `SignedRevocation` and the
  `RevocationSet` of them a device holds ([revocations](revocations.md)).
  [Node author numbers](credential-primitives.md#principal-ranges-and-node-authors),
  issuer authority and partition selectors use a versioned capability extension.
  `rocky-secure` re-exports these types.
- `Credentials { publisher, revocations, chain }::verify(device, app, time)`
  checks a whole supplied set for one application and returns the identity,
  grants, revocation digest and expiry. Nothing needs to be contacted.
- `IdentityProvider` supplies an [identity](#identities): its public key,
  signatures and author numbers. `FileIdentityProvider` keeps one in a
  directory for tests and servers; a `PrivateKey` signs as itself.
- `Authority { identity, chain }` is what an identity with issuer
  authority signs: `Authority::owner` for the owner, whose identity key is the
  publisher key, `Authority::issuer` for an issuer through its chain. It
  issues identity capabilities (`issue`, `issue_delegable`, `issue_terms`),
  issuer authority (`issue_issuer`), renews one with the same terms
  (`renew`), signs revocations (`revoke`) and endorses statements
  (`endorse`). `CredentialChain::certify` is an identity's node certificate,
  `CredentialChain::delegate` a narrower link to another identity.
- `Grant` is a [raw grant](credential-primitives.md#raw-grants); `Profile`
  produces raw grants and resolves them over named resources.
- `KeyProvider`, `FileKeyProvider` and `Vault` hold secrets.
- Enrollment, renewal and self-enrollment messages without a transport:
  `Invitation`, `PairRequest`, `seal_enrollment`/`open_enrollment`,
  `RenewRequest`, `Renewal`, `IdentityRequest` and `IdentityAnswer`.

Verification is necessary, never sufficient. A secure runtime verifies every
frame, session, sync request and transfer against the revocations it learned,
whichever issuer, transport or enrollment produced the credentials. Errors are
`Denied` (final for the same input), `Protocol` or `Provider`.

## Identities

Permissions belong to identities; each node signs under its own key,
certified by its identity. A chain runs publisher → issuer identities →
identity → node certificate. See
[phase 13](../features_09/13-identities-and-domain-credentials.md).

| Type | Key | Grants |
| --- | --- | --- |
| `Publisher` | The owner identity's key: the domain is it and the application | Signs root capabilities and the owner's statements |
| `Identity` | An `IdentityProvider`'s key, such as a passkey's | What its capability names; with issuer authority, issues, renews, revokes and endorses |
| `SigningDevice` | Node key, certified by its identity | Nothing beyond its identity's: signs operations, session frames and requests; owns author logs |
| `TransportEndpoint` | Route key | Nothing: whoever answers must prove a signing device |
| `ReaderScope` | Encryption scope at a key epoch | Reveals every row sealed under that key |
| `CoordinatorControl` | Tenant control key | Registers and controls coordinator domains; no row, key or command right |
| `ExternalLogin` | A provider's name for a person | Nothing by itself |

- **The provider signs, it never exports.** `IdentityProvider` has
  `public_key`, `sign(purpose, message)` and `reserve_author(range)`; the
  purpose (node certificate, capability, revocation, statement) lets a
  provider show or refuse what it signs. Core never sees how a key is
  stored or derived.
- **Node certificates** are the chain's leaf: subject the node's credential
  alias, the node reference, at most one author number of the identity's
  range and the identity's grants, never more. An identity with issuer
  authority names the grants its node holds.
- **Issuer authority** is the extension's `issues` flag on an identity's
  link. Only the publisher's root or an issuer link sets it. Its holder may
  grant any grant, partition and author range below it; delegations
  decrease strictly, so issuers nest at most six
  deep and an issuer renews only identities below it.
- **Endorsements** sign the domain's statements: manifests, transitions,
  seal-based existence bases and discovery values carry an `Endorsement`, the
  signer's issuer links (none for the owner) and its signature. `verify`
  checks authenticity whatever happened later; `verify_current` also needs
  the signer's chain current under a revocation set, which is what adopting
  a newly met generation uses.

A `LoginProvider` verifies an external assertion into an `ExternalLogin`. An
issuer may use that to decide whether to issue a capability to an identity;
the assertion carries no grant, scope key or executor eligibility. No
provider ships with Rocky.

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
| `issuer` | Writer of every collection, plus issuer authority | All |

In the SDK, an `issuer` also writes and a `custody` peer holds the reserved
[issuer registry](issuer-registry.md); no other profile or raw grant names it.

Every grant selects whole rows. No capability names a generation's
manifest: grants name tables and fields by their name-derived ids
([decision 0067](../decisions/0067-names-are-identities.md)), so a chain
holds in every generation and adopting one renews nothing
([adopting generations](adopting-generations.md)). A grant's fields are
`FieldSelection::All`, every field of the table in any generation, added
later included, or `Only` the named ones; a profile or raw grant without
`columns` grants `All`, custody `Only` none. With [typed commands](typed-commands.md), a
writer also gets `Invoke` for the commands whose arguments are sealed for a
collection it writes, and an issuer `Execute` for every command; each is
paired with the command's manifest item. No profile holds `PublishSchema`;
`Execute` for a device that is not an issuer is a raw grant.
A profile naming no or an unknown collection is refused; there is no default.
In JSON a profile is `{"kind": "reader", "scopes": ["items"]}`, `"writer"` the
same, or `{"kind": "issuer"}`; unknown fields are refused.

## Key providers

Key providers hold an installation's secrets; an identity comes from an
[identity provider](#identities), which may keep it anywhere.


`KeyProvider` has `load`, `store` and `delete` by name; names use only
`[a-z0-9._-]`. A provider never invents a secret: loading an absent name is
`None`. `Vault` scopes names to one installation as `rocky.<installation>.<role>`.
`get` fails for a missing secret, `create` refuses to overwrite one, and only
the invitation token is replaced. A lost or deleted key is therefore an error
at the next use, never a silently regenerated identity. The SDK's `SecretStore`
is this trait; the Kotlin `SecretStore` adapts an application's storage to it.

The runtime calls `load` on its own thread, often several times a second, and
waits for it: a store answers from memory and pays any decryption once. `store`
returns after the secret is durable.

`FileKeyProvider` is the fallback: one owner-only file (`0600`, directory
`0700`) per secret. Its threat model:

- Secrets are **plaintext at rest**. File permissions protect them from other
  users of the machine, not from the owner, an administrator, malware running
  as the owner, a backup or a copy of the disk.
- Supply a `SecretStore` backed by the platform's protected storage where one
  exists. The application chooses and owns it; the SDK ships no Android store.

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

An issuer revokes one node or an identity. `revoke` with `node` names that
node's certificate; other nodes of its identity keep working. `revoke` with
`identity` names the identity and every node and capability its records
list: every chain through it ends, unrecorded nodes included. `revoke` with
`trust` names one trusted grant by its capability id; only the owner revokes
one, and since it reads nothing no key rotates
([trusted identities](tips-and-bookmarks.md#trusted-identities)). The owner's identity is never revoked.
The intent is saved in `identity.json` first: the
[revocation statement](revocations.md) and the key epoch each collection and
group partition the nodes could read rotates to, the scope's held maximum
plus one. Both come from the nodes' [registry](issuer-registry.md) records;
a partition-only record rotates only the group partitions its grants select.
Without a record every held collection key and every known group partition
secret this issuer holds rotates, including an unrecorded delegate or a node
that enrolled itself. Then it:

1. creates each of those key epochs;
2. signs the statement with its identity through its issuer chain and adds
   it to its set, which ends the named chains and no other;
3. renews its own chain, saves the identity with its intent still present,
   and teaches the replica;
4. commits the tombstone into each named node's [registry](issuer-registry.md)
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
an unknown user, identify the delegator, or prevent a fresh node
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
- **Issuers.** Revoking an issuer identity ends every chain through it: it
  signs nothing that verifies where the statement is known, and the
  identities it granted renew at another issuer. No issuer ever held the
  publisher key. A revoked issuer's own statements signed while its chain
  was valid still count: two issuers revoking at once both count; two that
  revoke each other are both revoked.
- **Chains through issuers** expire no later than the issuer above them.
  Only the owner, or an issuer above, renews an issuer; the owner's
  identity provider is the domain's backup.
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
- Not implemented: login providers, keystores other than Android's, the
  passkey identity provider.

## Formats

| Record | Version | Content |
| --- | --- | --- |
| `identity.json` (`rocky_sdk` `VERSION`) | 17 | See [SDK enrollment](sdk-enrollment.md#persisted-formats); the node's `identity` and whether the secret store holds it, a pending `rotation` names its `nodes`; 16 is refused |
| `mailbox.json` | 1 | A mailbox's deposits by digest: depositor, recipient node key, arrival, the sealed delivery or the recipient's receipt ([decision 0054](../decisions/0054-envelope-delivery.md)) |
| Sealed delivery, delivery receipt | 1 | Labels `rocky.envelope-delivery.*.v1` and `rocky.envelope-receipt.v1` ([decision 0054](../decisions/0054-envelope-delivery.md)) |
| Issuer registry rows | | One row per entry of a device's record ([decision 0048](../decisions/0048-replicated-issuer-registry.md)) |
| Invitation URI `rocky://join/` | 5 | Carries the grant profile and an explicit `via_custody` flag; it grants to the joining identity |
| Pair request | 3 | Labels `rocky.credentials.pair-*.v3`; `user` is the joining identity. The enrollment carries profile, key epochs, the enrolling issuer's chain and, for an issuer, an author range; never a key. Optional `routes` are bound by the proof when present |
| Renewal | 4 | Request signed by the node (`rocky.credentials.renew.v2`), bound to an issuer audience and a one-time X25519 key; answer (`rocky.credentials.renewal.v4`) signed by the answering identity, carrying its issuer chain and revocation set and sealing `{keys, issuers}` to that key |
| Identity request | 1 | `rocky.credentials.identity-request.v1`, signed by the new node and its identity for an answering node; the answer (`rocky.credentials.identity-answer.v1`) is sealed to a one-time X25519 key |
| Capability extension | 4 | `authors`, `device`, `node`, `partitions` and `issues`; see [the chain format](credential-primitives.md#chain-format) |
| Author binding | 2 | `rocky.credentials.author-binding.v2`, with the link's `issues` flag |
| Endorsement | | The signer's issuer links and signature in `SignedManifest` (`rocky.signed-manifest.v2`), `SignedExistenceBase` (`rocky.existence-base.v1`, format 1) and `DiscoverySecret` (`rocky.discovery-secret.v2`) |
| Contact URI | 2 | `issuer` and `enrollment_only` flags; `issuer` written only by issuers |
| Capability | 2 | Signed under `rocky.capability.v2`, id `rocky.capability.id.v2`; no manifest; extension body 4 ([chain format](credential-primitives.md#chain-format)) |
| Revocation statement and set | 3 | See [revocations](revocations.md#formats) |
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
- An issuer is an identity with issuer authority, signing through its own
  chain; the publisher key stays with the owner's identity, so a provider
  that derives it, such as a passkey, is its backup and a revoked issuer is
  fully excluded. Nodes of one identity each sign their own author logs.
- Rotation never seals history again: it would rewrite signed bytes and could
  not retract what a revoked reader already holds. Ciphertext names its epoch.
- Revoking an identity key, not only a capability ID, ends capabilities
  another issuer signs later for that identity. Revoking a node reference
  ends only that node's certificate.
- `authority.rotation` is saved before any key is made or anything is signed,
  so an interrupted revocation completes with the same keys and statement.
- An issuer never revokes itself, and the owner's identity is never
  revoked: a statement naming the publisher key is refused.
