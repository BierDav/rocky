# Scope: an equal-issuer, roster-free mesh

Status: agreed on 2026-10-09, with the open questions
[decided](#decided-2026-10-09) after a review against the code. Implemented by
phases 5 to 16 and 20 of [the roadmap](README.md).
Owner: `rocky-credentials` policy and envelopes; `rocky-secure` sessions,
frames and key epochs; `rocky-sdk` enrollment, sync and discovery;
`rocky-iroh`; the Kotlin surface and the shopping example.

## Problem

A member is tied to the trusted node that enrolled it
([enrollment](../runtime/sdk-enrollment.md#renewal-revocation-and-logout)):

- Each issuer keeps its own device registry in `identity.json`. It is not
  replicated, so only the enrolling issuer knows a device's user key,
  capability and profile.
- A member syncs with its configured peers, at most 16, starting with its
  inviting issuer. Nothing is discovered
  ([peers](../runtime/sdk-enrollment.md#peers-and-contacts)).
- The revocation policy is one linear chain of epochs. Two issuers revoking
  concurrently both sign epoch n+1 and receivers refuse the second as
  equivocation ([limits](../runtime/credentials.md#limits)).
- A horizon's statements and acknowledgments are not relayed by a keyless
  custody peer, so every participant meets an issuer
  ([horizon](../runtime/mergeable-horizon.md#declare-acknowledge-finalize)).
- A strong command is delivered to the executor among the caller's configured
  peers and refused when it is not one of them.

An issuer therefore revokes precisely only what it enrolled; another issuer's
device costs a rotation of every collection. Applications compensate with a
replicated roster. The shopping example grants every member read on
`devices`, `keys` and `identities`, so a partial device learns every device's
name, kind and node key, and which device received which list.

## What I need

### 1. Equal issuers

- **One registry.** Every device's user key, node key, capability ID and
  expiry, profile, partition grants, assigned pseudonyms and command identity
  are replicated to every issuer and to nobody else. An issuer acts on any
  device from it.
- **Any issuer renews.** A member renews at whichever issuer it reaches.
  `remove_peer` protects no last issuer.
- **Revocations merge.** The revoked set only grows. A revocation is one
  signed statement; a policy is the set of statements a device has learned.
  Two issuers revoking at once produce two statements, and every device ends
  with both. There is no equivocation between honest issuers.
- **Rotations merge.** Two issuers rotating one scope at once create two new
  key epochs. Every reader opens both. Writers converge on one by a
  deterministic rule over data both epochs carry, and the next rotation
  supersedes both.
- **Concurrent revocations rotate again.** Each of two concurrent rotations
  was sealed without knowing the other's revocation, so each may have reached
  the device the other revoked. An issuer that holds an epoch sealed under a
  smaller revocation set than it now knows rotates once more. Until then the
  scope is exposed to those devices, and that window is stated.
- **Precision.** Revoking any device rotates only the collections and group
  partitions its registry record could read, whichever issuer revokes.

### 2. Admission without a roster

A member keeps a deny list, not an allow list. A peer is an admissible sync
partner when it presents a valid credential chain for the domain and neither
its node, user nor capability is in the revocation set this device knows.
Grants still bound what is served. A member needs no directory of devices.

### 3. Discovery and partner choice

- Peers find each other on the local network and through a topic that only
  unrevoked domain members can derive. Routes learned this way are local contacts.
  They are never written to a replicated table.
- A sync ranks the peers it knows: local first, then those that serve what
  this device needs, then trusted nodes. A trusted node is required only for
  renewal and enrollment.
- Discovery belongs to the attached network. An application-owned endpoint
  keeps its own discovery configuration; TCP and custom transports have none
  and keep working from contacts.

### 4. Holdings only after authentication

Which partitions a device holds is never broadcast. Inside an authenticated
session a peer answers only for partitions the asking peer proves it holds:
by a proof derived from the partition's current key for a reader, by its
custody grant for a keyless custody peer. A device without either learns
nothing about that partition, including whether it exists.

### 5. Key delivery

A sealed envelope travels to its recipient alone: inside a session with the
recipient, or through a mailbox on a peer that holds it sealed until the
recipient asks. No replicated table records who received which partition.

### 6. Horizon and strong commands

- A horizon finalizes on a participant that met only a custody peer. The
  declaring issuer names participants from the registry.
- A caller reaches the executor without having it as a configured peer.
  Executor takeover behaves when two issuers act concurrently.

### 7. The applications

- Members drop their read grants on roster tables. The shopping example
  removes `devices`, `keys` and `identities` from member grants; a member
  shows only **This device**.
- Device management, including the full Devices tab, stays on trusted nodes
  and reads the registry.

## Decided 2026-10-09

- **The whole mesh is in this roadmap**, not only equal issuers.
- **Strong commands are qualified in the mesh**, through a restored market
  example ([phase 19](19-market-example.md)), not left to a later roadmap.
- **No backward compatibility.** Every format here replaces its predecessor.

The questions the original idea left open, answered after reviewing the code.
The owning brief repeats each under "Settled" with its consequences.

| Question | Decision | Phase |
| --- | --- | --- |
| Whether a revoked issuer's statements count | Always. A statement counts when signed under the publisher authority; two issuers revoking each other are both revoked. Any other rule depends on arrival order | [6](06-revocation-set.md) |
| Which earlier frames a device accepts once it learns a revocation | A frame is refused when its author is in the set or its chain expired; a statement's time never enters admission. Rows of a revoked author that a device did not yet hold arrive by checkpoint, as today | [6](06-revocation-set.md) |
| How large the set grows | 1,024 live statements. A statement leaves the distributed set once every chain it ends has expired; the registry's tombstone is permanent and stops the user renewing | [6](06-revocation-set.md) |
| How a member learns a revocation from a peer it never met | Every handshake compares a digest of the set after authenticating the chain; the side that lacks statements receives them before any row, and a revoked device is told why | [9](09-roster-free-admission.md) |
| How long a concurrent second key epoch stays readable | Under the existing rule: until no retained operation was sealed with it ([retaining keys](../runtime/credentials.md#retaining-historical-keys)). Nothing retires automatically | [8](08-concurrent-rotation.md) |
| The writers' tie-break between two concurrent epochs | The epoch whose secret hash is lower. It only settles which epoch writers use meanwhile; an epoch sealed without a revocation of one of its readers is superseded by a further rotation | [8](08-concurrent-rotation.md) |
| What proves a holding | A value derived from any epoch of the partition the answering peer retains; toward a keyless custody peer, knowledge of the partition tuple | [10](10-holdings-after-authentication.md) |
| What a mailbox is | Any admitted peer with a custody grant, and every issuer, whatever the partition. The sender deposits. The mailbox sees the recipient's node key and neither the group nor the secret; bounded per recipient | [12](12-direct-key-delivery.md) |
| Whether the discovery topic changes on revocation | Yes. It derives from values that travel with revocation statements inside admitted sessions, never with renewal and never to the revoked device. A device also answers the previous topic for a bounded window | [14](14-contacts-and-partner-choice.md), [15](15-discovery.md) |
| How a caller finds the executor | It asks the peers it meets for the executor's contact and dials it, through a relay where needed. Nothing is forwarded | [14](14-contacts-and-partner-choice.md), [16](16-strong-commands-in-the-mesh.md) |
| How many devices a domain holds | 256 in the registry; a horizon still names at most 64 participants | [5](05-issuer-registry.md), [13](13-relayed-horizon.md) |

## Not requested

- **Recovery from a stolen issuer.** A revoked issuer keeps the publisher
  key. Revocation recovers from loss, not theft; excluding a thief still needs
  a new domain ([issuer recovery](../runtime/sdk-enrollment.md#issuer-recovery)).
- **A guaranteed remaining issuer.** Two issuers that revoke each other
  concurrently are both revoked. A domain left without an issuer is the
  existing unrecoverable case.
- **Anonymity.** A session peer still sees the other's credential chain, and a
  network observer still sees which endpoints talk
  ([transports](../runtime/sdk-transports.md#iroh)).
- **Hiding timing or sizes** from a relay or custody peer.
- **Rotating pseudonyms.** The registry stores several per device and table so
  that [rotation](../ideas/pseudonym_rotation.md) needs no format change.
- **Retraction.** A revoked device keeps what it decrypted.
- **Multi-hop forwarding of rows** beyond what custody already does, and
  forwarding of a command or its result through a peer.
- **A gossip overlay.** Discovery publishes and resolves records; it relays
  no application data.
- **Readmission.** A revoked user stays revoked; enrolling it again needs a
  new user key.

## Acceptance

1. Two issuers, partitioned, each revoke a different device; the two devices
   read different scopes, the same-scope case being item 3. After they meet,
   every device holds both revocations; neither issuer's statement is refused.
2. Issuer B revokes a device issuer A enrolled, A being offline. Only the
   scopes that device could read rotate; the device opens no later row.
3. Two issuers rotate one list at once. A reader opens rows sealed under
   either epoch; after one sync every writer seals under the same one.
4. A member enrolled by A renews at B while A is gone for good, and removes A.
5. A member admits a peer it has no contact for, and refuses one whose
   revocation it learned from a third peer in the same round.
6. Two members on one network find each other with no trusted node and no
   custody peer reachable, and converge.
7. A peer without list L's key or custody grant receives no frontier, offer or
   refusal that distinguishes "L exists" from "L does not".
8. A member's replica contains no node key, name or kind of another device,
   and receives the envelope for a newly shared list. Command identities in
   an application's own rows are not node keys and may remain.
9. A horizon finalizes on a device that met only the custody peer.
10. A caller that never met the executor submits a command and reads its
    outcome.
