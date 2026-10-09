# 0048: Replicated issuer registry

Status: accepted.
Date: 2026-10-10.

## Context

Each issuer kept the devices it enrolled or renewed, their partition grants
and the pseudonyms it assigned in its own `identity.json`. Another issuer knew
a device only after renewing it, so revocation, renewal and author numbers
depended on which issuer a device happened to meet
([equal issuers](../features_07/scope-mesh.md#1-equal-issuers)). The registry
replaces these maps with one replicated collection that issuers alone read.

## Decision

**The collection.** Every SDK domain carries a reserved mergeable table,
`issuer_registry`, with text columns `node`, `entry` and `value` and the
primary key `(node, entry)`. The SDK appends it to the application's
definition before building the logical schema, so it is part of the signed
manifest like any table, with its own reader scope
(`rocky.sdk.readers.v1` over its name). An application schema that declares a
table of that name is refused. It is not a resource of grant profiles or raw
grants: the SDK adds its grant itself, read and write to the `issuer` profile
and ciphertext-only custody to a `custody` profile, and nothing else names it.
Application SQL sessions and driver connections exclude its scope; sync
includes it.

**The key.** Its reader key is an ordinary collection key: `reader.issuer_registry`
in the secret store, listed in `identity.json` `keys`, sent in the enrollment
of an issuer and in renewal key lists, and rotated by the ordinary revocation
of a device whose profile is `issuer` (that profile reads every collection).
There is no other delivery or rotation path.

**The record.** One device's record is every row whose `node` is its
lowercase hexadecimal node key. `value` is JSON of one entry:

| `entry` | `value` | Written |
| --- | --- | --- |
| `device` | `user` key, `kind` (`created`, `paired`, `granted`, `delegated`, `partition`, `renewed`), `profile` (absent when only partition grants are known), enrolling `issuer` node key, `command` identity (hex `NodeId`), `enrolled` time | Once; a later `grant` of another profile replaces it |
| `revoked` | `{}`: the tombstone | Once, never deleted |
| `capability/<id>` | capability `id` and its `expires` | Per issued chain; entries with an earlier expiry are deleted |
| `grant/<digest>` | a partition `grant` and its `delegations` depth | Once per distinct grant |
| `pseudonym/<table>/<key>` | `table` commitment, pseudonym `key`, `author` number, `assigned` time, `closed` (stored, never set) | Once per assigned key |
| `label` | `label`: base64 of at most 256 opaque application bytes | Updated in place |

**Merge rules** fall out of row identity: each entry is its own natural key,
and the visible incarnation per key is the greatest creation stamp
([row identity](../runtime/logical-schema.md#row-identity)). Records merge per
node key. The tombstone is inserted and never deleted, so it only becomes set.
Distinct capabilities are distinct rows and readers take the later expiry.
Grant and pseudonym lists are rows keyed by their content, so they merge by
union. Two issuers writing the same `device` entry concurrently leave one
visible entry; two concurrent label writes resolve by field registers.

**The bound** is 256 records with a `device` entry per domain, replacing the
64 devices and 64 holders one issuer knew. A write that would add the 257th
fails with `registry_full` and writes nothing; the pairing that needed it
fails before its invitation is consumed.

**Ordering.** Pairing, renewal, `grant`, partition grants and delegations on
an issuer, and pseudonym assignment commit their rows in one secure
transaction before the answer leaves. An issuer writes the registry under its
own pseudonym for `issuer_registry`, assigned from its own author range on its
first write. A retried pseudonym request at any issuer returns the number the
registry holds for that key.

`identity.json` 10 drops `authority.devices`, `authority.holders`,
`author_assignments` and per-device author ranges. An issuer keeps the claim
of its current invitation (node, user and author range) so a lost answer
retries as the same device, and a begun revocation keeps a snapshot of the
revoked record so it completes before the replica opens.

## Consequences

- Any issuer renews, lists and records any device once the record synced; an
  issuer that has not synced it renews from the presented chain and writes it.
- `remove_peer` no longer protects a member's last issuer; `peers` and
  readiness report a member without issuer contact.
- A device another issuer recorded concurrently with a different `device`
  entry keeps one of the two; both name the same user and node.
- A non-issuer that assigns author numbers from a delegated range keeps no
  assignment record: a retried request keeps its number only by presenting the
  previous chain.
- Revocation reads the registry record but still rotates as before; precise
  rotation for another issuer's devices is [phase 7](../features_07/07-registry-revocation.md).
