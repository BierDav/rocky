# 14. Contacts and partner choice

Status: not started.
Owner: `rocky-sdk` contacts, sync targets and the sync worker; `rocky-secure`
session; Kotlin `peers()`. Depends on [phase 2](02-change-driven-sync.md) and
[phase 9](09-roster-free-admission.md); lands last on the session, after
[phase 10](10-holdings-after-authentication.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [discovery and partner choice](scope-mesh.md#3-discovery-and-partner-choice),
what was [decided](scope-mesh.md#decided-2026-10-09),
[peers and contacts](../runtime/sdk-enrollment.md#peers-and-contacts) and
`targets()` and `learn_issuers` in `crates/rocky-sdk/src/enrollment.rs`.

`targets()` returns every configured peer with a route, issuers first, and a
pass dials them all. A device learns a contact only from the application or,
for issuers, from an enrollment or renewal answer. Let devices pass contacts
on, ask for one, choose the best partners, and hold the value from which
[phase 15](15-discovery.md) derives its topic.

## Settled

- Each revocation statement comes with a 32-byte discovery value minted by
  the revoking issuer and signed under the publisher authority together with
  the statement ID, so no peer can substitute one.
- A value is handed over only inside an admitted session, after the
  revocation check on the merged set, never to a device the set revokes and
  never with enrollment or renewal. The domain discovery root travels the
  same way; a new device receives it in its first admitted session.
- The topic derives from the root and the values of the live statements a
  device holds, ordered by statement ID and using the cutoff of the set
  digest. Equal sets derive equal topics.
- A device also answers the previous topic for 24 hours after it derives a
  new one. A device that has not caught up is found by contacts.
- A caller finds a node by asking admitted peers for the contact of a node
  ID. The answer is a hint: the session authenticates whoever answers.
- Ranking reads the held and refused replies of the frontier exchange, which
  phase 10 made proof-bound. Nothing about holdings is persisted.

## Ordered slices

1. Decision record 0056: the contact family of the session, the discovery
   value and root, and the topic derivation.
2. One new session message family in phase 9's module layout, with three
   exchanges: contact handover, contact request, and discovery values beside
   the revocation statements.
3. Discovery value and root: minting in `revoke`, verification, storage in
   the secret store and not in `identity.json`, hand-over, and an internal
   call returning the current and the previous topic. Nothing uses the topic
   until phase 15.
4. Handover: a session passes on at most 16 contacts of peers it reached
   since its last successful pass, within the existing limits of 4 routes
   and 2,048-byte addresses. It passes none for a node in the revocation set.
   Issuer contacts in enrollment and renewal answers stay as they are.
5. Request: a peer asks for the contact of a 16-byte node ID, as
   `secure_node_id` derives it, at most 8 per session. The answer is the
   contact or "unknown", the same for a revoked node and one never met. The
   asker checks that the answered node key hashes to the ID.
6. Learned contacts enter the bounded list of phase 9, marked with their
   source, aged and evicted. A learned contact never evicts one the
   application added. None is written to a replicated table.
7. Ranking in place of issuers-first: contacts marked local, then peers
   whose last frontier replies held scopes this device needs, then trusted
   nodes; peers in the per-peer back-off of phase 2 last. A pass dials at
   most phase 9's per-pass limit and stops adding peers once every needed
   scope was held by one it reached. Renewal still dials an issuer. The
   local mark is set by phase 15 and, until then, by the test hub.
8. `peers()` and its Kotlin shape report each contact's source and why the
   last pass chose or skipped it.

## Completion gate

- A device with one contact learns a second peer's contact by handover and a
  third's by request, and syncs with both.
- With a peer marked local and a trusted node both reachable and both
  sufficient, the pass syncs with the local peer alone and reports why.
- A device needing a partition only the custody peer holds reaches it within
  two passes.
- A CI run shows a revoked device refused at admission before any contact,
  value or root is sent, and admitted peers receiving no contact for it.
- Two devices holding equal statement sets derive equal topics; a device
  missing one statement or its value derives a different one; a substituted
  value is refused.
- A full contact list evicts the oldest learned contact and keeps configured
  ones.
- No replicated table holds a learned route, a value or the root; the
  replica is searched for each.
- An unreachable peer costs no dial while its back-off lasts.
- [Enrollment](../runtime/sdk-enrollment.md#peers-and-contacts) describes
  contacts, requests and ranking.

## Not in this phase

- Announcing or looking up anything on a network, the transport port and
  `rocky-iroh` ([phase 15](15-discovery.md)).
- Changing the enrollment, invitation or renewal formats.
- Forwarding rows for a peer and multi-hop routing; forwarding a command is
  [phase 16](16-strong-commands-in-the-mesh.md).
- Persisting or replicating which peer holds what.
- Raising the contact or per-pass limits of phase 9.
- Electing a preferred peer, or discovering coordinators.
