# 3. Group secrets

Status: not started.
Owner: `rocky-credentials` envelopes; `rocky-sdk` key store, sharing,
revocation and subscriptions; Kotlin `partitions` API. Depends on
[phase 2](02-partition-groups.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the group secret and its consequences in
[the scope](scope-partitioned-constraints.md#1-partition-groups), decision 0037
and [partition credentials](../runtime/partition-credentials.md). Today each
table and partition has an independent random key, one envelope per recipient
and its own rotation. Depth must cost no additional key delivery.

The replica keeps one key per table scope and refuses the same secret under
two scopes. That stays: the SDK derives a distinct key per table and installs
it as today. Frames, logs, checkpoints, grants and pseudonyms do not change.

## Ordered slices

1. Derivation: table key from the group secret, bound to the domain, group
   address, table identity and epoch. A derived key reveals neither the
   secret nor a sibling key.
2. Envelope: wraps one group secret for one recipient, bound to the group
   address and epoch. It replaces the per-table envelope.
3. Install: the SDK accepts an envelope only with whole-partition read on
   every table of the group, stores the secret in the installation vault and
   installs every table's derived key. A table added to the group by a later
   schema generation gets its key from the held secret.
4. Create and share: creating a list prepares one secret; `share(group,
   values, recipients)` returns one envelope per recipient.
5. Revocation: one saved intent rotates the secret; every table's epoch
   advances together. An interruption resumes to a state where all tables of
   the partition are on the same epoch.
   Revocation must cover every partition the device could read, not only the
   collections of its enrollment profile. Today `Settings::rotate` skips a
   partition whose table the stored profile does not read, and `partition_grant`
   records nothing on the issuer, so a device granted a partition after
   enrollment keeps reading it after revocation. Required:
   - the issuer records each partition grant and delegation against the
     device and its user subject, durably and before the chain is returned;
   - rotation covers every recorded group partition of the revoked device, and
     every known group partition when the issuer never recorded the device;
   - revoking a device revokes its user subject even when the issuer knows it
     only through a partition grant, so a fresh node key cannot renew;
   - `share` refuses a revoked recipient and a non-current epoch.

Known gaps in `rocky-sdk/src/node/partition.rs` and `rocky-credentials`; the
search is not complete:

- `register_partition` runs before the envelope is opened and its grants are
  verified, so a bad envelope leaves a registered address; `vault.put` runs
  before the epoch-capacity check and can orphan a secret. Verify first.
- `partition_create` starts at epoch 1 while rotation uses the policy epoch, so
  a create after a rotation installs below the current epoch.
- Rotation creates each key locally with `vault.create`; a second issuer holds
  no key for the new epoch until it receives an envelope and can rotate only
  the partitions it knows. The group secret, not the issuer, is the unit.
- `rocky-credentials/src/partition.rs` hardcodes the sealed length for a
  32-byte secret.
6. Holding rule: subscribe, unsubscribe, evict and bootstrap take a group
   partition. An application may leave a named table out explicitly, for
   example large attachments on a phone. Leaving out a table that a held
   table of the group references is refused, since that child could never
   resolve.

## Completion gate

- Sharing a list with three tables needs one envelope per recipient; the
  recipient reads all three.
- A recipient lacking read on one table of the group cannot install the
  secret.
- After revocation the removed device opens no row written afterwards in any
  table of the group; remaining readers do, from one new envelope each.
- A device enrolled without read on a table, then granted one of its
  partitions, opens no row written there after its revocation; the same holds
  when the issuer knows the device only through that grant, and its user key
  cannot renew under a new node key.
- Sharing to a revoked recipient is refused.
- A crash at every step of a rotation recovers to one epoch across the group.
- A table added to the group later is readable without a new envelope.
- A phone holds a list and its items while leaving out a third table.
- [Partitioned scopes](../runtime/partitioned-scopes.md) and
  [partition credentials](../runtime/partition-credentials.md) describe it.
