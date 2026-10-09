# A full mesh with equal trusted nodes

Status: scheduled on 2026-10-09 in [the mesh scope](../features_07/scope-mesh.md),
which is the authority and answers the open questions below.

## Background

A member is tied to the trusted node that enrolled it:

- Each issuer keeps its own device registry (`authority.devices` in its
  `identity.json`). It is not replicated, so only the enrolling issuer knows a
  device's user key, capability and profile.
- A member's issuers are usually just its inviting issuer. It renews there, and
  `remove_peer` refuses to drop the last one.
- A member syncs only with its configured peers. `targets()` adds the registry
  devices only on an issuer.
- The revocation policy is one linear chain of epochs. Two issuers revoking
  concurrently both sign epoch n+1, and peers refuse the second as
  equivocation.

As a result, a trusted node can revoke only the devices it enrolled or renewed
precisely. Revoking another issuer's device costs user-key revocation and
narrow rotation.

Applications also tend to publish a household roster. The shopping example
grants members read access to `devices`, `keys` and `identities`. A partial
device learns every device's name, kind and node key, and which device holds
which list.

## Goal

- Trusted nodes are equal in power: any of them can enroll, renew, revoke and
  rotate for any device. They may still differ in the data they hold, since a
  trusted node may be partial.
- Any device can sync with any other admitted device, preferring the best
  partner rather than a fixed issuer.
- No device learns the household roster. A device knows only the peers it
  actually meets.

## Equal issuers

This comes first. A gossip transport alone leaves members tied to their
issuer.

1. **Replicated issuer registry.** Every device's user key, capability ID,
   profile and assigned pseudonyms live in a collection that only issuers
   read. Any issuer can then renew, and can revoke with user-key revocation
   and rotate only the collections the device's profile could read.
2. **Mergeable revocations.** The revoked set is grow-only. Concurrent
   revocations from different issuers merge instead of competing for the next
   epoch. A policy names the revocations it includes rather than a single
   predecessor.
3. **Concurrent rotation.** Two issuers that rotate one collection at once
   create two new key epochs. Readers accept both. Writers converge on one
   epoch through a deterministic tie-break, such as the issuer key.
4. **No issuer is special.** A member renews at any issuer it can reach, so
   `remove_peer` has no last issuer to protect.

## Admission without a roster

A member keeps a deny list, not an allow list. A peer is an admissible sync
partner when it presents a valid credential chain for the domain and is not in
the signed revocation set. Members already receive that set with the policy,
so they need no directory of devices.

## Discovery and partner choice

- Peers find each other through local discovery or a gossip topic that only
  domain members can derive. Routes learned this way are kept as contacts, not
  published.
- `targets()` ranks the peers it has met by proximity (LAN first) and by
  whether they serve the partitions this device needs.
- A trusted node is contacted only when no closer peer can serve, or for
  renewal and key delivery.

## Holdings only after authentication

Advertising which partitions a device holds lets others choose it, but also
reveals what it holds. Holdings are therefore never broadcast:

- They are exchanged only inside an authenticated handshake between admitted
  peers.
- A peer answers only for partitions the asking peer proves it holds, for
  example by a proof derived from the partition's key at a current epoch. A
  device without the key learns nothing about that partition.

## Key delivery

Sealed envelopes go directly to their recipient, through a mailbox or the sync
handshake, instead of through a replicated table every member reads. Envelopes
are already sealed. This change removes the remaining metadata: who received
which partition.

## Application consequences

- Members drop read grants on roster tables. The shopping example would remove
  `devices` and `identities` from its member grants, and members would show
  only **This device**.
- Device management, including the full Devices tab, stays on trusted nodes,
  which read the replicated issuer registry.

## Open questions

- How a member learns about a new revocation from a peer it has never met
  before, without a roster: does the revocation set travel with every
  handshake?
- How long a concurrent second key epoch stays readable once writers have
  converged.
- Whether the gossip topic should rotate with the policy epoch so that revoked
  devices can no longer derive it.
