# 9. Roster-free admission

Status: implemented on 2026-10-10 with
[decision 0051](../decisions/0051-roster-free-admission.md). Acceptance 5 and
the device revoked while offline are `cargo test -p rocky-sdk --test admission`
(about 1 s alone); foreign, expired and self-signed chains, the revoked peer,
another session version, the 1,024-statement exchange and the 300-stranger
bound are `rocky-secure` `tests::admission`; the 64 strangers against the
inbound limit are `rocky-sdk` `node::sync_tests::admission` (about 0.5 s
alone). Equal sets of 1,024 statements exchange 440 bytes after the hellos;
1,024 against none move 719,214 bytes, every page within a frame. The phase 6
interim refusal is gone; its acceptance test now passes without it.
Owner: `rocky-secure` native session; `rocky-core` protocol names; `rocky-sdk`
serving, peers and sync; the Kotlin `peers()` shape. Depends on
[phase 6](06-revocation-set.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Rebase verification

The phase 8 rebase boxes checkpoint pages without changing their Serde encoding.
Issuers retain the epoch-delivery path rather than starting a member rebootstrap
when a handshake brings revocations. The three sibling-rotation tests keep their
assertions and pass. `rocky-secure` builds at development `opt-level = 3`; the
unchanged frame-encoding test measured 8.627 ms alone, and the two catch-up tests
with existing exceptions measured 1.463 s and 1.820 s. These meet the ordinary
10 ms/2 s budgets without relying on those exceptions.

## Inputs and objective

Read [admission without a roster](scope-mesh.md#2-admission-without-a-roster),
the [decisions](scope-mesh.md#decided-2026-10-09),
[peers and contacts](../runtime/sdk-enrollment.md#peers-and-contacts),
[permissions and sync](../runtime/permissions-and-sync.md), the
[transport bounds](../runtime/sdk-transports.md#bounds) and decisions
[0044](../decisions/0044-group-frontier-exchange.md) and
[0046](../decisions/0046-per-scope-frontier-refusal.md).

A device serves only node keys it configured; an issuer also serves devices it
recorded (`admitted()` in `rocky-sdk` enrollment). The handshake in
`rocky-secure/src/network.rs` proves the node key only: the chain rides in
`Hello` unverified and is checked on each request, and `accept` and
`authenticate` take no replica. Both sides send their hello at once, so
whoever connects is shown the acceptor's chain. Phase 6 leaves an interim: the
hello carries the set digest and a mismatch is a named refusal that triggers
one renewal. Admit by chain and revocation set, and let the set travel inside
the session.

## Settled

- Handshake stages: the initiator's hello, then the acceptor's, then both
  signatures; the chain check without revocation; set digests; on a mismatch
  paged statement IDs, then paged statements; the revocation check on the
  merged set. An outsider is shown no chain, and a revoked device receives the
  statement that revokes it before it is refused.
- Statements use the delta codec of phase 6. Byte strings are CBOR byte
  strings and every page fits one 256 KiB frame; the set holds at most 1,024
  statements.
- One session version constant yields the transport name
  (`rocky/native-session/N`) and the signing label. A session change adds a
  message module and raises the constant. There is no negotiation: another
  version is refused in the handshake.
- A service serves 32 inbound streams; further ones are refused and counted.
- Per-request verification stays. No session caches authority.

## Ordered slices

1. Decision record 0051. Put the transport name in `rocky-core` and the
   signing label in `rocky-secure` behind one constant. Split `Message` and
   the `answer` match into one module per family (rows, checkpoints, horizon,
   frontiers, existence, commands, revocations), as `network/frontiers.rs`
   already does. No message changes meaning in this slice.
2. Handshake as settled. `accept` and `authenticate` take the replica, verify
   the chain for this domain (publisher, manifest, generation policy, expiry)
   and learn statements through it. Remove the interim digest of phase 6 from
   the hello and its refusal.
3. Revocation at the entry point. A peer the merged set revokes is refused
   after the exchange. A side that finds itself revoked ends the session;
   `rocky-sdk` records the notice as it does after a refused renewal and
   reports `revoked`.
4. Serving: `admitted()` no longer filters. A session is admitted when the
   chain verifies and no statement revokes its node, user, capability or
   pseudonym. Grants bound every request as today. A custody peer and a
   non-issuing member serve by the same rule; custody forwarding of
   enrollment is unchanged.
5. Bounds for strangers. No row is written before the chain verified.
   `secure_horizon_credentials` holds one chain per node and
   `secure_peer_frontiers` one row per peer, scope and author: cap both at
   256 unconfigured peers per replica and evict the least recently served,
   never a configured peer or full node. A lost report only delays pruning.
   State each bound in the transport bounds.
6. Peers become contacts: a local, bounded list of routes that grants
   nothing, in `identity.json`. Split the limit of 16: 64 contacts kept, 16
   peers dialed per pass, configured ones first. `addPeer` keeps working.
7. `peers()` and its Kotlin shape list a peer met whether or not it was
   configured, with a `configured` flag.

## Completion gate

- [Acceptance 5](scope-mesh.md#acceptance) is a CI run on the in-process hub:
  C admits B without a contact for it, and refuses D in the round in which A
  told it of D's revocation.
- A device revoked while offline learns it from the first admitted peer it
  meets, reports `revoked` and stops syncing; no issuer runs in the test.
- A chain for another domain, an expired chain and a self-signed chain are
  refused with no row in `secure_horizon_credentials` or
  `secure_peer_frontiers`, and the initiator's transcript holds no chain of
  the acceptor.
- 64 credentialed strangers connect at once: 32 are served, 32 are refused
  and counted, and a configured peer syncs afterwards. After 300 distinct
  strangers both tables hold at most 256 unconfigured peers.
- With 1,024 statements equal on both sides the handshake exchanges digests
  only; its bytes and time are recorded. With 1,024 against none, every page
  fits a frame and the sets end equal.
- A build with another session version is refused in the handshake with the
  existing authentication error.
- [Enrollment](../runtime/sdk-enrollment.md),
  [permissions and sync](../runtime/permissions-and-sync.md),
  [transports](../runtime/sdk-transports.md) and
  [replica roles](../runtime/replica-roles.md) describe it.

## Not in this phase

- Finding or ranking peers, back-off and learned contacts
  ([phase 14](14-contacts-and-partner-choice.md),
  [phase 15](15-discovery.md)).
- The discovery value that phase 14 sends beside the statements.
- What a peer may learn about holdings
  ([phase 10](10-holdings-after-authentication.md)).
- Reading the registry: admission needs the chain and the set only.
- Version negotiation, a fallback reader or a frozen session name.
- A session-wide authority cache, or any change to grants and their meaning.
- The statement format, its bound and expiry
  ([phase 6](06-revocation-set.md)).
