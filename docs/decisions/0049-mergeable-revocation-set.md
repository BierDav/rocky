# 0049: A mergeable revocation set replaces the policy epoch

Status: accepted.
Date: 2026-10-10.

## Context

The policy was one statement signed with the publisher key: an epoch number
and a revoked set. A capability was current only while its epoch equalled the
policy's, so every revocation ended every chain in the domain, and two issuers
signing the same next epoch were refused as an equivocation. Two issuers could
not revoke at once, and a device that learned a revocation refused honest
history it had not received yet. [Phase 6](../features_07/06-revocation-set.md)
replaces the policy by statements that merge; its "Settled" answers are the
product decisions this record implements.

## Decision

**Statement.** `rocky_credentials::Revocation` names the revoked node, user and
pseudonym keys and capability IDs, the issuer's node key, a time and the
application (`AppId`, publisher and name) it belongs to. The publisher key
signs it (`rocky.revocation.v1`) and the issuer's node key co-signs it
(`rocky.revocation-issuer.v1`) for attribution only: a statement counts when
both signatures verify, also when its issuer is itself revoked. An encoded
signed statement holds at most 2,048 bytes. Its ID is the hash of the
statement (`rocky.revocation.id.v1`); Ed25519 signing is deterministic, so a
statement signed again has the same ID. Binding to the application, not to one
manifest, keeps the set across [generations](../runtime/generations.md).

**Set.** `RevocationSet` is the statements a device verified, in ID order.
Merging is union: a statement that fails verification, names another
application or exceeds its size refuses the whole merge, which changes
nothing. Order, overlap and repetition never refuse. `digest(time)` hashes the
IDs of the live statements (`rocky.revocation-set.v1`), so two holders of the
same live statements share it whatever order they learned them in. A set is
bounded to 2 MiB encoded.

**Cutoff.** A credential link may be valid for at most `MAX_CHAIN_LIFETIME`
(366 days) plus `LIFETIME_SKEW` (one day); verification refuses a longer one.
A statement is live while `time / DAY` is below `expiry_day`, the day on
which `statement time + 366 days + skew` falls, rounded up. Merging drops
statements past that cutoff and the digest leaves them out. Every chain a
statement could end has expired by then.

**Frame rule.** A chain is current when no link's subject is a revoked key and
no link's ID a revoked capability, and its validity covers the time. Nothing
compares a statement's time with a frame. A device that learns a revocation
refuses the revoked author's frames, earlier ones included, and admits frames
it lacked from every other author, whenever they were signed. Rows of a
revoked author it did not hold arrive by checkpoint. Confirmed command
results and historical checkpoints keep their exception: they verify with no
revocation at the time they were issued.

**Bound.** `revoke` refuses with `RevocationSetFull` (`revocation_set_full`)
once 1,024 live statements are held, before anything is saved or rotated. A
merge has no count bound below the 2 MiB size; two partitioned issuers near
the bound may together exceed 1,024.

**Tombstones.** An issuer records every node and user key of every statement
it learns in `authority.tombstones` of `identity.json`, and refuses their
renewal for good, also once the statement left the set. Its next commit to the
[issuer registry](0048-replicated-issuer-registry.md) also writes the revoked
records' tombstones, which other issuers read.

**What took the policy epoch.** Each reach of the epoch takes the set digest,
or nothing where the epoch only expressed "current":

| Reach | Becomes |
| --- | --- |
| Capability and pseudonym credentials (`epoch`, its equality in `links`) | Nothing; the frame rule above |
| Group creation | Nothing: a new group partition starts at secret epoch 1 |
| Key epoch of a revocation | The scope's held maximum plus one |
| Horizon statement `policy_epoch` | `revocations`, the declaring issuer's digest; format 3 |
| Checkpoint statement `policy_epoch` | `revocations`, the signer's digest; format 7. A changed digest refreshes the checkpoint, as a key epoch does |
| Existence base | Its ID covers the horizon's digest |
| `secure_generations` | Nothing: the `policy` column is gone; every generation verifies under the one set |
| Command history's synthesized statement | An empty set at the time of issue |
| Bootstrap intent token | Nothing: `true`, written when a renewal brought statements the device lacked |
| Session status, readiness and activity `epoch` | `revocations`: the digest and the live statement count |
| Recovery inventory `policy` | Nothing; format 3 |

**Session.** Until [phase 9](../features_07/09-roster-free-admission.md) the
hello carries the sender's digest (`rocky.native-session.v8`, transport name
`rocky/native-session/8`). After authenticating, each side refuses a peer with
another digest as `revocation set differs`; the SDK reports it as
`revocations_differ` and its sync renews once, which brings the statements.

**SDK.** `revoke(node)` builds the statement from the device's registry
record (node, user, capability and pseudonym keys) and saves an intent holding
the unsigned statement and the key epoch of each collection and group
partition it rotates, chosen by the record's profile and partition grants, then makes the
keys, signs the statement and renews the issuer's own chain. Phase 7 retains
that intent until the registry tombstone transaction commits; discarding the
JSON redo intent follows the commit. The existing tombstone is the completion
marker, so a stop between those writes replays the same keys and statement
and keeps one tombstone. No cross-file atomic write is claimed. Any stop completes at the next open with the same statement ID and
keys. Renewal answers (`rocky.credentials.renewal.v3`, transport name
`rocky/sdk-renew/2`), enrollments, supplied credentials and the
`rocky.sdk.policy.v1` answer carry the whole set; receivers merge it.
`revoked.json` version 2 holds the statement that revokes the device.

## Consequences

- Two issuers revoke concurrently; their devices end with both statements and
  one digest, and no statement is refused for its order.
- A revocation no longer ends every chain: devices renew on their ordinary
  schedule, and a revoked author's rows are the only history a device must
  take from a checkpoint.
- Until [phase 8](../features_07/08-concurrent-rotation.md), two issuers
  rotating one scope at once still meet "key differs from retained crypto
  state" where the second key arrives. Concurrent revocations of devices that
  read disjoint scopes merge cleanly.
- An issuer rotates only group partitions whose secret it holds; it has no
  epoch to follow for another one.
- An issuer cut off from every other issuer for longer than the chain lifetime
  may readmit a user whose statement left the set before it learned it.
- Devices whose clocks straddle a day boundary near a statement's expiry hold
  different digests for up to a day and refuse each other's hello meanwhile.
- `identity.json` is version 11, the encrypted replica `rocky.encrypted-replica.v9`;
  every earlier one is refused and reinstalled.
