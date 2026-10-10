# Issuer registry

Implemented in `rocky-sdk` (`registry.rs`, `node/registry.rs`). Every SDK
domain carries one replicated record per device that only issuers read and
write, so any issuer enrolls, renews, grants and revokes from the same facts.
The format, merge rules and bound are
[decision 0048](../decisions/0048-replicated-issuer-registry.md); enrollment
itself is in [SDK enrollment](sdk-enrollment.md).

## The collection

The SDK appends the mergeable table `issuer_registry` (`node`, `entry`,
`value`; key `(node, entry)`) to every application's definition, so it is in
the signed manifest and has its own reader scope. An application may not
declare a table of that name, and no grant profile or raw grant names it:

| Profile | Registry grant | Registry key |
| --- | --- | --- |
| `issuer` | Read and write | `reader.issuer_registry`, delivered and rotated like any collection key |
| `custody` | Ciphertext only | None: it relays the rows sealed |
| Others | None | None |

Application SQL sessions, driver connections and Room never see the table;
sync carries it between issuers and custody peers. `status` readable and
writable counts exclude it.

## A record

A device's record is every row whose `node` is its hexadecimal node key: its
`device` entry (user key, how it was recorded, profile, enrolling issuer,
command identity, enrollment time), a `revoked` tombstone, its capabilities
with expiry, its partition grants with delegation depth, its assigned
pseudonyms and an opaque label of at most 256 bytes. Each entry is its own row,
so records merge by [row identity](logical-schema.md#row-identity): the
tombstone only becomes set, the latest-expiring capability is current, and
grant and pseudonym lists merge by union.

## What commits before an answer

| Call | Saved in `identity.json` first | Then committed to the registry | Then returned |
| --- | --- | --- | --- |
| Pairing | Invitation attempts and claim, split author range | `device`, capability | Sealed enrollment |
| Renewal | Nothing | `device` when absent, capability | Renewal |
| `grant`, `delegate` on an issuer | Carved author range | `device`, capability | `supplied` |
| `partition_grant`, `partition_delegate` on an issuer | Nothing | `device` when absent, partition grants | `supplied` |
| Pseudonym assignment by an issuer | Author counter | Pseudonym | Chain |
| `revoke` | Intent with the statement and target epochs, then keys, the signed statement and tombstones | Tombstone | Statement ID and revocations |

Each commit also brings the issuer's own record up to date and writes a missing
tombstone for any device a held statement or tombstone revoked. An issuer writes under its own
`issuer_registry` pseudonym, assigned from its author range on its first
write. Its own pseudonyms for application tables join its record with that
commit. A lost answer leaves a record without a usable chain; the retry is the
same device: a pairing retry with the same nonce gets the claimed author range
again, a pseudonym request returns the number the registry holds for its key at
any issuer, and a renewal or grant records the new capability.

## Reading it

- `admitted()`: an issuer serves sync to its configured peers and every
  recorded device without a tombstone.
- `peer_add` on an issuer refuses a tombstoned contact; any device refuses a
  contact a held [revocation](revocations.md) names.
- Renewal refuses a node or user a held statement, a tombstone or the record
  revoked and renews
  an earlier generation into the record's profile. Without a record it renews
  the presented chain and writes one.
- `logout` counts recorded issuers besides `issuers`.
- `revoke(node)` builds its [statement](revocations.md) and target key epochs
  from the record and saves them as its intent. It rotates the
  collections the record's profile reads and the group partitions it reads or
  holds grants for, revokes its user, capability and pseudonyms, and rotates
  every held collection key and every known partition secret held here when
  there is no record. A device recorded only by partition grants selects
  only those grants. It needs no previous contact with this issuer.
  The tombstone transaction completes revocation; the JSON redo intent is
  discarded only afterwards, and interrupted completion resumes on open.

## Calls

| Request | Result |
| --- | --- |
| `devices` | Every other recorded device: `node`, `user`, `kind`, `issuer`, `enrolled`, `revoked`, `profile`, `capability`, `expires`, `command`, `label` (bytes), `partitions`, `pseudonyms` (issuer) |
| `device_label` with `node` and `label` bytes | Sets the label of a recorded device (issuer) |

Kotlin `RockyDatabase.devices()` returns `Device` with `expiresAtEpochSeconds`,
`command` and `label`; `setDeviceLabel(node, label)` writes the label on an
issuer.

## Limits

- 256 records with a `device` entry per domain; a tombstone keeps its record.
  The 257th pairing, grant, renewal or partition grant fails with
  `registry_full` and writes nothing; an invitation is not created then.
- Two issuers recording one device at once keep one `device` entry; both name
  the same user and node.
- A non-issuer assigning author numbers from a delegated range keeps no record:
  a retried request keeps its number only by presenting its previous chain.
- Non-issuer delegations remain unrecorded. Their recipients use full rotation
  when revoked by node key; no delegator, user or pseudonym is inferred.
  [Durable provenance](../ideas/delegation_provenance.md) is a future idea.
- Not covered: rotating pseudonyms (the `closed` field is stored, never set)
  and showing any registry field to a member.
