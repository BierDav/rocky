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
| `revoke` | Intent with the record, then policy and keys | Tombstone | New epoch |

Each commit also brings the issuer's own record up to date and writes a missing
tombstone for any device its policy revoked. An issuer writes under its own
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
  contact its policy revoked.
- Renewal refuses a node or user its policy or the record revoked and renews
  an earlier generation into the record's profile. Without a record it renews
  the presented chain and writes one.
- `logout` counts recorded issuers besides `issuers`.
- `revoke(node)` snapshots the record into its intent. It rotates the
  collections the record's profile reads and the group partitions it reads or
  holds grants for, revokes its user, capability and pseudonyms, and rotates
  everything when the registry holds no profile. A device recorded only by
  partition grants has no profile. Precision for another issuer's devices is
  [phase 7](../features_07/07-registry-revocation.md).

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
- Not covered: rotating pseudonyms (the `closed` field is stored, never set)
  and showing any registry field to a member.
