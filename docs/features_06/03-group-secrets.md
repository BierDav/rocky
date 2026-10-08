# 3. Group secrets

Status: complete on the assigned base; all completion gates passed on 2026-10-09.
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

## Implementation and evidence

`rocky-credentials::group` derives table keys by HKDF-SHA256 and replaces the
per-table envelope with `GroupSecretEnvelope`; the sealed length follows from the
secret length. `rocky-sdk::groups` resolves group partitions against the current
generation, stores one secret per epoch in the installation vault and derives
member keys on open, install, renewal and rotation. SDK identity version 9 holds
group records and issuer partition holders; version 8 is refused.
[Decision 0039](../decisions/0040-group-secrets.md) pins both formats. Kotlin
`partitions` takes group partitions: create, share, install, register, subscribe,
bootstrap, unsubscribe, evict and status.

The listed gaps are closed: install opens the envelope and verifies read on
every member before registering or storing; capacity is checked before a secret
is written; create uses the partition's current epoch, else the policy epoch;
rotation makes one new secret per group partition, keyed or not.

Gate evidence (`crates/rocky-sdk/tests/group_secrets.rs` unless noted):

- One envelope per recipient; recipients sync and read all three tables over the
  in-process transport (`readers_open_every_table_from_one_envelope...`).
- Missing read on one member refuses install with no secret, epoch or
  registration left (`one_envelope_shares_a_group...`).
- After revocation the remaining reader opens post-revocation rows in every
  table from one new envelope; the removed one holds no new-epoch key or secret.
- A device enrolled to read only `notes`, then granted list A, rotates A (not B);
  a grant-only device rotates every known partition; its user cannot be granted
  under a new node key, and renewal under a new node key is refused
  (`enrollment::tests`).
- Sharing to a revoked recipient or a non-current epoch is refused.
- Key-provider failure injected at every operation of a revocation, then reopen:
  every member is at epochs [1, 2] with exactly two secrets.
- A table joining the group in generation 2 derives its key; one row is sealed
  and read without a new secret.
- A partial-role phone holds lists and items with attachments left out; unknown
  or all-table omissions are refused; the closure rule is unit-tested
  (`groups::tests`) because partitioned references stay refused until phase 5.

Verification on parent `1a918842`: `./scripts/check` passed (752 passing test
executions, 15 ignored); `./sdks/native/check` passed; `./kotlin check` passed
(51 tests, including a new group test); `./kotlin test -m schema-qualification`
5 passed; `./kotlin check qualify -m schema-qualification` seven pairs passed;
`./kotlin test -m room-qualification` 12 passed;
`./kotlin check packaged -m packaged-consumer` passed.

Not verified: a revoked device receiving post-revocation frames through a custody
relay (it is refused sync by the issuer; the evidence is key-level), a second
issuer's rotation of a partition it holds only by address, device and real
network runs, and the four-table measurement of phase 9.

