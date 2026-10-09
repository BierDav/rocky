# SDK enrollment, keys and renewal

Implemented in `rocky-sdk` (`enrollment.rs`) and exposed through the native ABI
and Kotlin `RockyDatabase`. Enrollment is optional plumbing over
[credentials](credentials.md) (grant profiles, issuers, key epochs):
credentials made elsewhere install without it. It runs on
[secure sessions](secure-sessions.md) and [secure SQL](secure-sql.md) and
reaches peers through a [transport](sdk-transports.md). Qualified on loopback
only; not a cryptographic audit.

Enrollment dialing tries every supplied route with its own 15-second deadline.
After a route connects, the request and reply have a separate 30-second deadline;
two stalled routes do not prevent trying a later route.

## Domain and identities

A schema's collections become typed secure tables in one domain. Each
collection is its own reader scope with its own key epochs.

| Key | Held by | Purpose |
| --- | --- | --- |
| Publisher | Issuers | Trust anchor: signs manifest, policy and user capabilities |
| User | Each device | Subject of the capability the publisher issues |
| Node | Each device | Signs operations, session frames and renewal requests |
| Pseudonym, per table | Writing devices that request one | Signs as a table author; number and sealed device binding are in its credential |
| Transport | Each device | Stable key of its routes where the transport has one; no authority |
| Reader, per collection and epoch | Devices whose grants read it | Encrypts and decrypts that collection's rows |
| Invitation token | Issuers | Secret of the active invitation |

The creating device is the first issuer. Each device is enrolled as a
[grant profile](credentials.md#grant-profiles): reader or writer of named
collections, custody-only holder ([always-on peer](always-on-peer.md), no
reader keys or publishing authority), or issuer. An issuer enrolls, renews
and revokes; no issuer needs to serve every sync.

## Author installation

Kotlin `RockyDatabase.create` and `join` install the current author pseudonym
of every writable table before returning when an issuer answers. Issuers
assign their own authors; members send a signed pseudonym request over the
renewal lane. `installAuthors()` retries missing or expired assignments and
returns those newly installed; already-current assignments are unchanged.
Partition-selector grants use the same installation path without requiring
collection-wide rights; read-only devices request no authors.
Member sync retries missing authors. If no issuer answers a member's join,
the database remains open and assignments retry during sync; inserts requiring
an author fail until it is installed. Raw local/supplied composition remains
explicit and can call `installAuthors()` itself.

The `authors` native request returns `installed` entries with `collection`
and `author`. Assignments and counters are persisted before the issuer
answers, so a lost reply retries the same device's assignment. The
`rocky.sdk.author.v1` prefix distinguishes this request from ordinary renewal
and generation offers; it carries the existing signed `PseudonymRequest`.
Issuers verify its grants, manifest, epoch and device binding through the same
[author assignment](credential-primitives.md#author-pseudonyms) boundary.
There is one current request format and no legacy reader.

## Secret storage

Keys never enter replicated tables or `identity.json`. They live in a
`SecretStore` ([key provider](credentials.md#key-providers)) under
`rocky.<installation>.<role>`: reader keys use `reader.<collection>` for epoch 1
and `reader.<collection>.<epoch>` after; the author-assignment key
`author-issuer`; table signing keys `pseudonym.<collection>`.

```kotlin
val store = object : SecretStore {
    override fun load(name: String): ByteArray? = keychain.get(name)
    override fun store(name: String, secret: ByteArray) = keychain.put(name, secret)
    override fun delete(name: String) = keychain.remove(name)
}
val database = RockyDatabase.create(directory, schema, secrets = store)
```

Without a store, keys are owner-only plaintext files in `secrets/` of the data
directory. On Android, `KeystoreSecretStore(context)` seals them
([Android](android.md#keystore-secret-store)); no desktop keychain adapter
ships. Opening without a needed key fails; no key is made in its place. Local
rows are plaintext at rest.

## Persisted formats

| Record | Version | Content |
| --- | --- | --- |
| `identity.json` | 9 | Pinned publisher key, signed manifest/policy/lineage, credential chain, profile, held key epochs, group partition secrets and issuer grant holders, author ranges and assignment counters, issuers, peers with routes, `enrollment_routes` and optional custody `enrollment_upstream`; an issuer adds its invitation, known devices and a pending revocation |
| Invitation `rocky://join/` | 4 | Issuer routes, publisher key, profile, secret token, expiry, explicit `via_custody` flag |
| Contact `rocky://peer/` | 2 | Publisher key, node key, routes, issuer and `enrollment_only` flags |

Each record accepts only its listed version, independent of routing flags.
Other versions are refused before installation writes; identity files and keys
are never migrated. Reinstall with the current build. Assignment records and counters are saved before a
credential is returned.

## Invitations and pairing

`pairing(profile)` on an issuer returns a `rocky://join/…` URI; treat it as a
secret until used.

- It lasts ten minutes and enrolls one device. A new one replaces it. Ten
  wrong tokens lock it.
- The joiner generates its own user and node keys and proves the token. The
  issuer answers with a capability, the current reader keys the profile reads
  and, for an issuer profile, the publisher key, sealed under a token-derived key.
- The joiner verifies manifest, policy and capability against the invitation's
  publisher key, and that grants equal the profile. It pins the issuer's node key.
- A join persists its nonce and keys before contacting the issuer, so a lost
  answer retries as the same device; nobody else can reuse the invitation.
- A different schema, or no route of the attached kind, fails without
  consuming the invitation.
- An issuer knows up to 64 devices.
- A joining issuer sends its routes, bound by the token proof, so both issuers
  become peers. The enrollment carries contacts of other issuers, added as
  peers that also renew the joiner.

### Supplied credentials

`Database::device(directory, secrets)` makes a device's keys and returns their
public halves, the same ones when asked again. A publisher-key holder issues a
capability with `credentials::Authority`, or an issuer answers the `grant`
call with [raw grants](credential-primitives.md#raw-grants). `Database::install`
takes the result as `Supplied`: mesh and domain IDs, `Credentials`, reader keys
by collection and epoch, and renewing issuers' contacts. A chain ending at the
device's user is completed with its node key. Everything is verified first; a
chain for another device, a key for an unread collection or a missing reader
key installs nothing. No network is needed.

## Renewal, revocation and logout

Credentials last 30 days by default (`credential_ttl`, a minute to a year). A
sync renews them past half their lifetime: the device signs a timestamped
request for one issuer, which answers with the same grants under its current
policy and the current reader key epochs, sealed to a one-time key. Any issuer
renews any unrevoked device. An issuer renews itself after asking known issuers
for a newer policy. Expired credentials cannot open a member until an issuer
is reachable. Clock skew up to five minutes is accepted.

`revoke(node)` on an issuer starts a new key epoch for every collection the
device could read, and a new secret for every group partition it could read or
was granted, and signs the next policy epoch ending its chains and its user's
([group secrets](partitioned-scopes.md#group-secrets)). The
intent is saved first; an interruption completes at the next open. Others
renew on their next sync; the revoked device is refused sync and renewal and
keeps only what it already decrypted. See [key rotation](credentials.md#revocation-in-the-sdk)
and [limits](credentials.md#limits).

After a refused renewal, a device requests the issuer's public signed policy
with `rocky.sdk.policy.v1`. It verifies the pinned publisher, manifest, epoch
and revocation of its credential chain before reporting `revoked`. A verified
notice is kept in `revoked.json` (version 1), so readiness remains revoked
after reopening; sync returns the non-retryable `revoked` error. Earlier
decrypted data stays readable locally. Offline devices learn this only when
an issuer becomes reachable. A fresh invitation enrolls a new identity.

`logout()` ends sessions and deletes keys; `logout(purge = true)` also removes
local data on close. Rejoining needs a new invitation. An issuer that knows no
other issuer cannot log out: it holds the only known publisher key.

## Issuer recovery

If the only issuer is lost, nobody can invite, renew or revoke. Kotlin's
`pairDevice()` therefore pairs a user's further devices as issuers by default;
`pairDevice(issuing = false)` makes a writer of every table that cannot pair,
grant, delegate or revoke. A paired issuer holds the publisher key in its own
`SecretStore` with its own keys and an author range split from the enrolling
issuer's; no identity or author log is cloned.

Enrollments, renewal answers and `grant` bundles carry contacts of the other
issuers the answering issuer reaches; receivers add them as peers and issuers
within limits. A device enrolled before the second issuer learns it at its
next renewal. An issuer that refuses a sync from an unknown device first
answers the renewal that sync starts, then serves it.

When the creator is lost, a remaining issuer calls `revoke(node)` with its
node key: next policy epoch, ended chains, rotated reader keys. Others renew
from remaining issuers. An issuer cannot revoke itself; one issuer revokes at a time.

Unrecoverable:

- **A revoked issuer's publisher key.** Revocation recovers from loss, not
  theft; a stolen issuer needs a new domain.
- **The last issuer.** Members sync until credentials expire, then stop for good.
- **A device that knew only the lost issuer** and did not renew since: add a
  remaining issuer's contact with `addPeer`.
- **Writes under the ended epoch.** Peers that learned the revocation refuse
  them; renew, then write.

## Peers and contacts

A device syncs with its configured peers; a member starts with the issuer that
enrolled it. Two devices become peers by adding each other's contact:

```kotlin
val contact = phone.contact()          // rocky://peer/…, any channel
laptop.addPeer(contact.uri)            // and the phone adds the laptop's
laptop.peers()                         // node, routes, issuer, last sync state
```

A contact is not secret and grants nothing. A route is a hint: whoever answers
must sign the session with that node key and present a valid chain. Adding a
contact again replaces the routes of its transport kinds.

- A device serves only peers it configured; an issuer also serves devices it
  knows and did not revoke, and refuses contacts of revoked ones.
- Limits: 16 peers, 8 issuers, 4 routes per peer, 2,048-byte addresses.
- A member cannot remove its last issuer.
- Nothing is discovered or elected. Custody forwarding of enrollment
  ([issuer availability](always-on-peer.md#issuer-availability)) never
  substitutes custody routes for issuer row-sync routes.

## Sync

Each sync opens an authenticated native session with every configured peer,
pushes what the peer lacks and pulls what this device lacks for the granted
collections, in bounded frames. It succeeds when one peer was reached; `peers`
shows each. Members keep syncing while every issuer is offline, until their
credentials expire. Sync needs an [attached network](sdk-composition.md#attach-a-network);
reads and writes do not. Requests are authorized under the latest policy this
device learned. When a peer refuses, the device renews once and retries, which
is how it learns a revocation. Checkpoints and catch-up are in
[SDK bootstrapping](sdk-bootstrapping.md).

Kotlin `create`/`join`/`resume` default to `automaticSync = true`, which runs
`syncOnce` every 500 ms while foregrounded. `syncContinuously()` instead
enables the native [change-driven worker](always-on-peer.md#change-driven-sync);
`syncOnce` stays available.

## Native calls

`NativeSession.open(options, schema, secrets, commands)` takes `directory`,
`mode` (`create`, `join` with `invitation`, or `resume`) and `transport`;
`commands` composes [typed commands](typed-commands.md#calls). Calls use the
[SDK contract](sdk-contract.md) envelope. In Rust the same calls run on a
`Session` ([composition](sdk-composition.md)).

| Request | Result |
| --- | --- |
| `domain` | `scope`, `issuer`, `profile`, `collections`, `transport` (attached kind or empty) |
| `session` | `principal`, `node`, policy `epoch`, `expires`, `issuer`, `profile`, readable/writable counts |
| `pairing` with `profile` | `uri`, `expires`, `profile` (issuer) |
| `grant` / `delegate` with `device` and `grants` or `profile` | `supplied` for `install`; `delegate` narrows this device's grants |
| `install` with `supplied` | Replaces credentials; the new `session` |
| `devices`, `revoke` with `node` | Known devices with `profile` and `revoked`; the new epoch (issuer) |
| `renew` | The renewed `session` |
| `authors` | Newly installed table authors; see [above](#author-installation) |
| `activity`, `activity_next` | Bounded [background activity](activity.md) |
| `logout` with `purge` | Ends membership |
| `connection` with `online`, `sync` | `offline`, or `connected` with the number of `peers` reached |
| `contact`; `peer_add`, `peer_remove`, `peers` | Own contact `uri`, `node`, `routes`; configured peers with `issuer`, routes, `state`, `checked`, `error` |
| `network` | See [network status](sdk-composition.md#network-status) |
| `mutate`, `mutation_progress` | A durable create, set or delete by handle |
| `read`, `subscribe`, `next`, `unsubscribe` | Pages of a `SELECT` over the collections |

A mutation is one secure transaction, flushed before it is acknowledged; the
same handle reports its earlier commit. Errors carry `rejected`, `denied`,
`conflict`, `unreachable` or `storage` and retryability. Refused credentials
report `denied`; a missing key reports non-retryable `storage`.

## Qualification

`cargo test -p rocky-sdk` covers enrollment, convergence over TCP, iroh and a
supplied transport, durable handles, paging, subscriptions, renewal,
revocation and logout; `--test credentials` covers
[profiles and supplied credentials](credentials.md#qualification). `--test peers`:
members sync with the creator closed; unconfigured, removed and foreign-domain
peers are refused; a revoked device stays denied; three devices relaying edits
and deletes in several orders converge; a domain reopens on another transport
kind after one fresh contact; a contact pointing the creator's key elsewhere
reaches nobody; unknown identity and invitation versions are refused unchanged.
`--test custody` covers routed invitations and enrollment-only contacts.
`--test issuers` destroys the creator after pairing a second issuer, which
revokes it, invites a third device and renews earlier devices.
`./examples/shoppinglist-app/check` runs enrollment, direct sync, revocation, a
Kotlin `SecretStore` and `IssuerRecoveryTest` through Kotlin and Room.
Not covered: publisher key backup, login providers, desktop keychains.

## Design rationale

Status: current identity and routing formats supersede their compatibility
clauses as of 2026-10-08. Identity 9, invitation 4 and contact 2 each have one
reader; other versions require reinstall.

- Identity is transport-independent: a domain is its definition identity and
  publisher key, a device its node key under a credential chain. Routes are
  replaceable hints, so an address or transport change needs only a fresh
  contact.
- Contacts are unsigned. The session handshake proves the node key and the
  credential chain authorizes every request; a signature would only suggest
  that routes are trusted.
- Invitations and contacts each have one format, with explicit custody and
  enrollment-only flags that keep renewal routes separate from row sync.
- Peers are configured on both sides; nothing is discovered, elected or
  relayed for others, which keeps who serves whom auditable.
