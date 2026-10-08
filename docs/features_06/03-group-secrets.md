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
- A crash at every step of a rotation recovers to one epoch across the group.
- A table added to the group later is readable without a new envelope.
- A phone holds a list and its items while leaving out a third table.
- [Partitioned scopes](../runtime/partitioned-scopes.md) and
  [partition credentials](../runtime/partition-credentials.md) describe it.
