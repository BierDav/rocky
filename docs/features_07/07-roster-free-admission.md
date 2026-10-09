# 7. Roster-free admission

Status: not started.
Owner: `rocky-secure` native session; `rocky-sdk` serving, peers and sync.
Depends on [phase 5](05-mergeable-revocations.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [admission without a roster](scope-mesh.md#2-admission-without-a-roster),
the [proposed defaults](scope-mesh.md#proposed-defaults),
[peers and contacts](../runtime/sdk-enrollment.md#peers-and-contacts),
[permissions and sync](../runtime/permissions-and-sync.md) and the
[transport bounds](../runtime/sdk-transports.md#bounds).

A device serves only peers it configured; an issuer also serves devices it
knows. A member therefore needs a list of devices, and learns a revocation
only at renewal. Admit by credentials and the revocation set, and let the set
travel with every session.

## Ordered slices

1. Decision record for the next native session. The handshake authenticates
   both node keys and chains as today, then compares revocation set digests.
   The side lacking statements receives and verifies them before any other
   message. A side that finds itself revoked ends the session and records the
   notice as it does after a refused renewal.
2. Serving: a session is admitted when the chain verifies for this domain
   (publisher, manifest, generation policy, expiry) and no statement revokes
   it. Being a configured peer is no longer required. Grants bound every
   request as today.
3. Bounds for strangers: verify the chain before allocating per-peer state;
   bound concurrent sessions, peer-frontier rows and work per unknown peer so
   that a credentialed device cannot exhaust another. State each bound.
4. Peers become contacts: a local, bounded list of routes that grants nothing.
   Split the limits: contacts kept, peers dialed per pass. `addPeer` keeps
   working.
5. A custody peer and a non-issuing member serve by the same rule; custody
   forwarding of enrollment is unchanged.
6. Update `peers()` and its Kotlin shape: a peer met is listed whether or not
   it was configured.

## Completion gate

- [Acceptance 5](scope-mesh.md#acceptance) passes: C admits B without a
  contact for it, and refuses D in the round in which A told it of D's
  revocation.
- A device revoked while offline learns it from the first admitted peer it
  meets, reports `revoked` and stops syncing; no issuer is involved.
- A chain for another domain, an expired chain and a self-signed chain are
  refused before any per-peer state exists.
- 64 unknown credentialed peers connecting at once stay within the stated
  bounds and an honest peer still syncs.
- Handshake cost with a 1,000-statement set, equal on both sides, is recorded;
  equal sets exchange only digests.
- [Enrollment](../runtime/sdk-enrollment.md) and
  [permissions and sync](../runtime/permissions-and-sync.md) describe it.

## Not in this phase

Finding peers ([phase 9](09-discovery-and-partner-choice.md)) and what a peer
may learn about holdings ([phase 10](10-holdings-after-authentication.md)).
