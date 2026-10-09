# 5. Replicated issuer registry

Status: not started.
Owner: `rocky-sdk` enrollment, identity and node; `rocky-credentials` grant
profiles; `sdks/native` and the Kotlin `devices` surface. No dependency;
shares `identity.json` with [phase 6](06-revocation-set.md), in either order,
the second rebasing. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers),
[enrollment](../runtime/sdk-enrollment.md), its
[issuer recovery](../runtime/sdk-enrollment.md#issuer-recovery),
[grant profiles](../runtime/credentials.md#grant-profiles) and
[formats](../runtime/credentials.md#formats), and
`crates/rocky-sdk/src/enrollment.rs`, `groups.rs` and `node.rs`.

Each issuer keeps `authority.devices`, `authority.holders`,
`author_assignments` and each device's author range in its own
`identity.json`. Another issuer knows a device only once it renewed it there.
Make the registry one replicated collection that issuers alone read, and let
every issuer enroll, renew and grant from it.

## Settled

- The registry is a reserved SDK collection of the domain that only the
  `issuer` profile reads and writes. It is absent from application schemas;
  no other profile and no raw grant names it. A custody peer may hold it
  sealed.
- Its key is an ordinary reader key: it travels in the existing enrollment
  and renewal key lists and rotates through the ordinary revocation of a
  device whose profile is `issuer`. No new delivery or rotation path.
- Bound: 256 devices per domain, replacing the 64 devices and 64 holders one
  issuer knows today. Enrollment beyond it fails with a named error.
- Record, final for this roadmap: node key, user key, kind and profile,
  enrolling issuer, revoked tombstone, partition grants with their delegation
  depth, latest capability ID and its expiry, per table a list of
  {pseudonym key, author number, assigned-at, closed}, the device's command
  identity, and an opaque application label of at most 256 bytes. Later
  phases read these fields and add none.
- Records merge per node key. The tombstone only ever becomes set and is
  never removed; the capability ID with the later expiry wins; grant and
  pseudonym lists merge by union.
- `Settings::renew`, `pair`, `grant` and partition grants are synchronous
  over `identity.json` today. They move onto the replica, because the
  registry row must commit before the chain is returned. This move is the
  larger half of the phase.

## Ordered slices

1. Decision record 0048: the reserved collection, its place in the signed
   manifest, the record encoding, the merge rules and the 256 bound.
2. The collection and its record, with typed reads for the SDK. Issuers get
   their registry author from their own range; ranges stay disjoint because
   each issuer still splits its own at pairing.
3. Move the answering side of pairing, renewal, `grant`, partition grants and
   delegations, and pseudonym assignment from `Settings` onto the node, so
   each commits its registry row and only then returns the chain. A retried
   pseudonym request at another issuer returns the assignment the registry
   holds instead of a second author number.
4. Any-issuer renewal: the answering issuer needs no earlier contact with the
   device; it takes profile, grants and tombstone from the record. An issuer
   that has not yet synced the device's record renews from the presented
   chain as today and writes the record.
5. Readers of the old maps move to the registry: `admitted()`, the revoked
   check of `add_peer`, `another_issuer`, `revoked_device`, `record_holder`
   and the lookup `rotate` uses. `revoke(node)` keeps its present behavior
   over these reads; precision for devices another issuer enrolled is
   [phase 7](07-registry-revocation.md).
6. `remove_peer` no longer refuses the last issuer. A member with no issuer
   contact reports that in `peers` and in readiness.
7. `identity.json` takes its next version without `authority.devices`,
   `authority.holders`, `author_assignments` and `Device.authors`. Native and
   Kotlin `devices()` read the registry and expose the chain expiry, the
   command identity and the label; a setter writes the label on an issuer.
8. Guides: enrollment, credentials (correcting the invitation and contact
   versions its formats table lists, which are 4 and 2 in code) and the
   limits that named 64 devices.

## Completion gate

- A device enrolled at A is listed at B after one sync with the same profile,
  partition grants, pseudonyms, chain expiry and command identity.
- [Acceptance 4](scope-mesh.md#acceptance) runs in CI on the in-process hub:
  B renews a device it never met, A being stopped; the device removes A and
  keeps syncing.
- A member, a non-issuing writer and a custody peer hold no registry key and
  open no registry row; the custody peer relays the rows sealed.
- Two issuers enrolling at once produce disjoint author ranges and both
  records; two issuers renewing one device at once leave one record.
- Fault sweep over pairing, renewal and `grant`, stopping after the registry
  commit and before the answer is written, and after the answer is written
  and before the requester stores it: a record exists without a usable chain,
  never a chain without a record, and the retry yields the same device.
- The 257th enrollment fails with the named error and writes nothing.
- A registry of 256 devices syncs to a new issuer and reopens; both times
  are recorded in the enrollment guide.

## Not in this phase

- Revoking from another issuer's records and precise rotation
  ([phase 7](07-registry-revocation.md)).
- The revocation set and any change to the signed policy
  ([phase 6](06-revocation-set.md)).
- Admitting peers a member has no contact for
  ([phase 9](09-roster-free-admission.md)); only an issuer's `admitted()`
  changes its source here.
- Naming horizon participants ([phase 13](13-relayed-horizon.md)) and
  removing shopping's roster tables
  ([phase 20](20-shopping-without-a-roster.md)).
- Showing any registry field to a member, application data beyond the label,
  and rotating pseudonyms; the `closed` field is stored and never set.
- A second key delivery path for the registry, or recording delegations made
  on a non-issuer.
