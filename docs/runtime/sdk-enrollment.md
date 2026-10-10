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
| Publisher | Issuers | Trust anchor: signs manifest, revocation statements and user capabilities |
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
and revokes; no issuer needs to serve every sync. Issuers share one record per
device in the replicated [issuer registry](issuer-registry.md).

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
and `author`. The counter is saved and the assignment committed to the
registry before the issuer answers, so a lost reply retried at any issuer
returns the same number. The
`rocky.sdk.author.v1` prefix distinguishes this request from ordinary renewal
and generation offers; it carries the existing signed `PseudonymRequest`.
Issuers verify its grants, manifest, revocations and device binding through the same
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
| `identity.json` | 13 | Pinned publisher key, signed manifest, revocation set, lineage, credential chain, profile, held key epochs, group partition secrets, author range and counter, issuers, peers with routes and local source/age metadata, `enrollment_routes` and optional custody `enrollment_upstream`; an issuer adds its invitation and the device that claimed it, a pending revocation with its statement and target key epochs, and its tombstones |
| Invitation `rocky://join/` | 4 | Issuer routes, publisher key, profile, secret token, expiry, explicit `via_custody` flag |
| Contact `rocky://peer/` | 2 | Publisher key, node key, routes, issuer and `enrollment_only` flags |

Device records are not in `identity.json`: they are rows of the
[issuer registry](issuer-registry.md). Each record accepts only its listed
version, independent of routing flags.
Other versions are refused before installation writes; identity files and keys
are never migrated. Reinstall with the current build. Assignment records and counters are saved before a
credential is returned.

## Invitations and pairing

`pairing(profile)` on an issuer returns a `rocky://join/…` URI; treat it as a
secret until used. The result's `sequence` is its `InvitationCreated` activity
sequence for this open database. Applications can associate choices with the
subsequent `DeviceEnrolled` events before another invitation replaces it.

- It lasts ten minutes and enrolls one device. A new one replaces it. Ten
  wrong tokens lock it.
- The joiner generates its own user and node keys and proves the token. The
  issuer answers with a capability, the current reader keys the profile reads
  and, for an issuer profile, the publisher key, sealed under a token-derived key.
- The joiner verifies manifest, revocations and capability against the invitation's
  publisher key, and that grants equal the profile. It pins the issuer's node key.
- A join persists its nonce and keys before contacting the issuer, so a lost
  answer retries as the same device; nobody else can reuse the invitation.
- A different schema, or no route of the attached kind, fails without
  consuming the invitation.
- A domain's registry holds up to 256 devices; the 257th enrollment fails
  with `registry_full` before the invitation is consumed.
- Every joiner sends its available return route, bound by the token proof.
  The issuer keeps it as a contact and wakes sync, so later writes reach an
  idle member without polling. Only an issuer profile adds issuing authority.
  The 64-contact limit still applies. If it prevents learning a return route,
  arrange delivery through another contact or free a slot and add the
  recipient's contact. The enrollment also carries contacts of other issuers,
  added as contacts that renew the joiner.

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
request for one issuer, which answers with the same grants, the
[revocations](revocations.md) it holds and the current reader key epochs,
sealed to a one-time key. Any issuer renews any unrevoked device: it takes the
tombstone and profile from the device's registry record, or renews the
presented chain and records it when the record has not reached it yet. An
issuer renews itself after asking known issuers, which is how it learns their
revocations. Expired credentials cannot open a member until an issuer is
reachable. Clock skew up to five minutes is accepted.

`revoke(node)` on an issuer starts a new key epoch for every collection the
device could read, and a new secret for every group partition it could read or
was granted ([group secrets](partitioned-scopes.md#group-secrets)), and signs a
statement revoking its node, its user, its capability and its pseudonyms. The
intent is saved first; an interruption completes at the next open with the
same statement and keys. Selection comes from the replicated record, whoever
enrolled the device: a profile selects collections and group partitions;
a partition-only record selects only its grants. An unrecorded device,
including a non-issuer's delegate, rotates all held collection keys and known
partition secrets held here; only its node can be named without a record.
The tombstone commits before intent cleanup and before success is returned.
Open completes interrupted cleanup before serving sessions. Any issuer
revokes while others do: their statements
merge. Others learn it on their next sync, which a differing revocation set
turns into a renewal; the revoked device is refused sync and renewal and keeps
only what it already decrypted. See [key rotation](credentials.md#revocation-in-the-sdk),
[revocations](revocations.md) and [limits](credentials.md#limits).

After a refused renewal, a device requests the issuer's public revocation set
with `rocky.sdk.policy.v1`. It verifies the pinned publisher and application
and finds the statement that revokes its credential chain before reporting
`revoked`. That statement is kept in `revoked.json` (version 2), so readiness
remains revoked after reopening; sync returns the non-retryable `revoked`
error. Earlier decrypted data stays readable locally. Offline devices learn
this only when an issuer becomes reachable. A fresh invitation enrolls a new
identity; the revoked user key never renews again at an issuer that learned
the statement.

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
next renewal. An issuer serves every device its registry records; one that
refuses a sync from a device whose record has not reached it first answers the
renewal that sync starts, then serves it.

When the creator is lost, a remaining issuer calls `revoke(node)` with its
node key: a statement ending its chains, rotated reader keys. Others renew
from remaining issuers. An issuer cannot revoke itself.

Unrecoverable:

- **A revoked issuer's publisher key.** Revocation recovers from loss, not
  theft; a stolen issuer needs a new domain.
- **The last issuer.** Members sync until credentials expire, then stop for
  good. A member that removed every issuer reports `issuer_contact: false`.
- **A device that knew only the lost issuer** and did not renew since: add a
  remaining issuer's contact with `addPeer`.
- **The revoked issuer's own writes**, earlier ones included. Peers that
  learned the revocation refuse them; its rows they lacked arrive by checkpoint.

## Peers and contacts

A device serves any device of its domain that dials it: a peer is admitted
when its credential chain verifies and no revocation statement either side
holds revokes it ([decision 0051](../decisions/0051-roster-free-admission.md)).
It dials its contacts; a member starts with the issuer that enrolled it. Only
the dialing side needs a contact:

```kotlin
val contact = phone.contact()          // rocky://peer/…, any channel
laptop.addPeer(contact.uri)            // the laptop now dials the phone
laptop.peers()                         // source, choice, routes and last sync state
```

A contact is not secret and grants nothing. A route is a hint: whoever answers
must sign the session with that node key and present a valid chain. Adding a
contact again replaces the routes of its transport kinds. Grants still bound
every request.

- `peers` lists every contact and the peers met only through their own
  sessions, each with `configured`; a peer met that way has no routes.
- A device refuses a contact a held statement revokes; an issuer also one its
  registry tombstoned.
- Limits: 64 contacts, 16 selected per sync pass, 64 peers met listed,
  8 issuers, 4 routes per contact,
  2,048-byte addresses. The replica keeps horizon credentials and frontiers of
  at most 256 unconfigured peers ([transport bounds](sdk-transports.md#bounds)).
- Any contact can be removed, the last issuer included; it is no longer dialed
  and is still served. `peers` and readiness (`authority.issuer_contact`)
  report a member left without issuer contact.
- Sessions pass on at most 16 recently reached contacts and filter revoked
  nodes. Learned routes have a source (`handover`, `request`, `enrollment`, or
  `discovery`) and learning time. They age after 30 days and the
  oldest eligible learned contact is evicted at capacity. They never overwrite
  or evict an `application` contact. The forwarding upstream is retained.
- `requestContact(nodeId)` / native `contact_request` asks admitted peers for
  the contact of a 16-byte node ID, encoded as 32 hexadecimal characters.
  Each session allows eight requests. Unknown and revoked nodes both return
  null. A returned node key must hash to the requested ID; the next session
  authenticates that key before serving anything.
- On iroh, [discovery](discovery.md) announces the device under the domain's
  topic and adds what it finds, marked `local` when found on the local
  network. Custody forwarding of enrollment
  ([issuer availability](always-on-peer.md#issuer-availability)) never
  substitutes custody routes for issuer row-sync routes.

## Sync

A sync ranks local contacts first, then peers whose last proof-bound frontier
replies held needed scopes, then trusted contacts, then other probes. Backed-off
peers come last and are not dialed. A leading local peer is tried alone;
remaining attempts run in groups of at most four. Between groups, the pass
stops adding peers once successful frontier replies covered every needed
concrete scope. Holdings are not persisted. Renewal still dials an issuer.
`peers().source` names how the contact was learned; `peers().choice` explains
`selected_local`, `selected_holding`, `selected_trusted`, `selected_probe`,
`covered`, `backoff`, `pass_limit`, or `not_considered` for the last pass.

An explicit sync opens authenticated native sessions with the contacts it
dials,
pushes what the peer lacks and pulls what this device lacks for the granted
collections, in bounded frames. It succeeds when one peer was reached; `peers`
shows each. Members keep syncing while every issuer is offline, until their
credentials expire. Sync needs an [attached network](sdk-composition.md#attach-a-network);
reads and writes do not. Requests are authorized under the revocations this
device learned. Every handshake exchanges the statements either side lacks, so
a device learns a revocation from any admitted peer; one that learns its own
reports `revoked` and stops syncing. Checkpoints and catch-up are in
[SDK bootstrapping](sdk-bootstrapping.md).

Kotlin `create`/`join`/`resume` default to `automaticSync = true`, enabling and
observing the native [change-driven worker](always-on-peer.md#change-driven-sync)
while foregrounded. `syncContinuously()` enables the same worker and observes
its status. Both automatic and explicit passes respect per-peer back-off. An explicit pass
with only backed-off contacts reports `unreachable` without dialing. Connectivity
changes and an explicit worker restart reset it. The sync response shape is
unchanged. `enterBackground` cancels
the automatic pass within one second; `enterForeground` starts one immediately.

A session that brought revocation statements or refused credentials triggers
one renewal after the concurrent attempts finish: rotated keys arrive with it.
A pass renews first when a served session brought statements. Without an
issuer the rows still sync and the new keys wait. Successful old handshakes and credential
failures are then checked again against the refreshed authority, still at most
four at a time. Unavailable routes keep their retry deadline.

Change-driven delivery needs a route back to each recipient.
Pairing learns the joining device's authenticated return route when it supplies
one and the issuer has room in its peer set. Sessions hand over recently reached contacts; applications can still add them
explicitly. An idle recipient with only a one-way route needs a connectivity
signal or `syncOnce` to pull later remote writes.

The creator mints a discovery root, and every revocation mints a signed value
bound to its statement ID. Only admitted sessions exchange them, after merging
and checking revocations. Contact-family messages are encrypted even on TCP.
The root and values are structured secret-store bytes, never identity or
replicated rows, and are absent from enrollment and renewal answers. Equal
live statement sets and values derive equal topics; a missing value differs.
The internal `discovery_topics` call reports current and previous topics, with
24-hour previous-topic expiry. Phase 15 uses these for network discovery; see
[decision 0056](../decisions/0056-contact-exchange-and-discovery-secrets.md).

## Native calls

`NativeSession.open(options, schema, secrets, commands)` takes `directory`,
`mode` (`create`, `join` with `invitation`, or `resume`) and `transport`;
`commands` composes [typed commands](typed-commands.md#calls). Calls use the
[SDK contract](sdk-contract.md) envelope. In Rust the same calls run on a
`Session` ([composition](sdk-composition.md)).

| Request | Result |
| --- | --- |
| `domain` | `scope`, `issuer`, `profile`, `collections`, `transport` (attached kind or empty) |
| `session` | `principal`, `node`, `revocations` (`digest`, live `statements`), `expires`, `issuer`, `profile`, readable/writable counts |
| `pairing` with `profile` | `uri`, `expires`, `profile`, invitation activity `sequence` (issuer) |
| `grant` / `delegate` with `device` and `grants` or `profile` | `supplied` for `install`; `delegate` narrows this device's grants |
| `install` with `supplied` | Replaces credentials; the new `session` |
| `devices`, `device_label`, `revoke` with `node` | [Registry](issuer-registry.md#calls) records; a label set; the new revocation set (issuer) |
| `renew` | The renewed `session` |
| `authors` | Newly installed table authors; see [above](#author-installation) |
| `activity`, `activity_next` | Bounded [background activity](activity.md) |
| `logout` with `purge` | Ends membership |
| `connection` with `online`, `sync` | `offline`, or `connected` with the number of `peers` reached |
| `contact`; `peer_add`, `peer_remove`, `peers` | Own contact `uri`, `node`, `routes`; contacts and peers met with `issuer`, `configured`, routes, `source`, `local`, `choice`, `state`, `checked`, `error`, `issuer_contact`, and the [discovery status](discovery.md#status) |
| `contact_request` with hexadecimal `id` | Learned `node` key, or null |
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
`--test registry`: a device enrolled at A is listed at B after one sync with
the same profile, partition grants, pseudonyms, chain expiry, command identity
and label; a member renews at an issuer it never met while the first is gone,
removes it and keeps syncing (acceptance 4, in-process hub); a member, a
non-issuing writer and a custody peer hold no registry key or open row while
the custody peer relays the rows between two issuers; concurrent enrollments
at two issuers keep disjoint author ranges and both records, and concurrent
renewals one record; pairing, renewal and `grant` interrupted after the
registry commit, before and after the answer is written, retry as the same
device; the 257th device fails with `registry_full` and writes nothing.

Measured on 2026-10-10 on an Apple Silicon Mac in the debug test profile
(dependencies at `opt-level = 2`), two runs: filling a registry to 256 devices
with 253 grants took 21.9 s and 26.0 s; a new issuer that had settled its own
first sync pulled all 256 records in one sync in 16.1 s and 11.6 s, and
reopened in 2.5 s both times. Release builds were not measured.
`./examples/shoppinglist-app/check` runs enrollment, direct sync, revocation, a
Kotlin `SecretStore` and `IssuerRecoveryTest` through Kotlin and Room.
Not covered: publisher key backup, login providers, desktop keychains.

## Design rationale

Status: current identity and routing formats supersede their compatibility
clauses as of 2026-10-08; identity 10 moves device records to the
[registry](../decisions/0048-replicated-issuer-registry.md) as of 2026-10-10.
Identity 11 replaces its policy epoch with the
[revocation set](../decisions/0049-mergeable-revocation-set.md); its `peers`
are contacts, 64 of them, since
[roster-free admission](../decisions/0051-roster-free-admission.md).
Identity 13, invitation 4 and contact 2 each have one reader; other versions
require reinstall.

- Identity is transport-independent: a domain is its definition identity and
  publisher key, a device its node key under a credential chain. Routes are
  replaceable hints, so an address or transport change needs only a fresh
  contact.
- Contacts are unsigned. The session handshake proves the node key and the
  credential chain authorizes every request; a signature would only suggest
  that routes are trusted.
- Invitations and contacts each have one format, with explicit custody and
  enrollment-only flags that keep renewal routes separate from row sync.
- A member keeps a deny list, not an allow list: any device of the domain
  whose chain and the revocation set admit it is served, so no device needs
  a roster of the others. Contacts only say whom to dial; admitted sessions now exchange hints and
  secrets under [0056](../decisions/0056-contact-exchange-and-discovery-secrets.md).
  No node is elected and rows are not forwarded for others
  ([0051](../decisions/0051-roster-free-admission.md)).
