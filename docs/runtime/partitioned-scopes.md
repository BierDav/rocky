# Partitioned scopes

A table's partition columns are a confidentiality boundary inside its domain:
each partition has its own key, logs and checkpoints, because a subscription
cannot narrow what one key reveals. This guide is for an application using
schema declarations and the packaged SDK;
[partition credentials](partition-credentials.md) defines selectors and envelopes.

## Declare the boundary

```kotlin
val shopping = schema("shopping") {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id", group = "list") }
    table("items") {
        integer("list"); text("key"); integer("amount")
        primaryKey("list", "key"); partitionBy("list", group = "list")
    }
    view("partial_items", "SELECT * FROM items WHERE row_kind = 0")
}
```

Partition columns are immutable, non-null mergeable primary-key columns.
There are at most 16. Their declared affinity and collation canonicalize tuple
values, so SQL-equal values select the same scope. The identity index is the
partition columns followed by `id`, omitting a repeated `id`. A natural-key
Room entity must declare that unique index using `<table>_identity` as its name.
A primary key that already equals the identity needs no additional index.

A Room export supplies columns, keys, indexes and `@DatabaseView` declarations.
Apply `partitionBy` in the import overlay; do not edit the generated JSON.

```kotlin
schemaFromRoom("shopping", exportedJson) {
    table("lists") { partitionBy("id", group = "list") }
    table("items") { partitionBy("list", group = "list") }
}
```

## Partition groups

`lists.id` and `items.list` map to one group's slot by position. Every member
must have equal slot counts, affinities and collations; build and installation
refuse a mismatch naming both tables. Names are case-sensitive and unique in the
group namespace. Omitting `group` creates a private group named by the table;
another table cannot name that group. An unpartitioned table cannot join.
Group declarations affect canonical bytes and replicated identity.

`LogicalSchema.partition_groups()` exposes groups, ordered slots and member
tables to native runtime/SDK consumers. `group_address(domain, table, values)`
returns the same address across members for SQL-equal positional values,
including NOCASE and RTRIM spellings, independent of their column identities.
The address binds the domain and group name in slot declaration order.
[Decision 0037](../decisions/0037-partition-groups.md) pins its encoding.

Table scopes, logs, checkpoints, and grants stay per table; the key
comes from the [group secret](#group-secrets). Native partition-complete
`LatestWins` indexes resolve per partition;
[mergeable constraints](mergeable-constraints.md) describes their identities and
rebuild boundary. In-group references and references to an unpartitioned
logged parent are admitted. Partitioned `SetNull` indexes, references across
groups, existence logs and command control remain refused, by the Kotlin builder
and Room import as natively.

Changing an existing table's group or renaming a group is refused by generation
admission; a new table may join an existing group. A schema on a sealed fork
may remove a table. Its retained partition addresses still bind the exact
domain, table and canonical tuple; incoming history still requires its source
schema and signed credentials. Without the table's current descriptor, those
addresses create no allocation binding or rows. Restoring the descriptor checks
its partition columns again before restoring eligible births. See
[admission evidence](admission-evidence.md).
Changing partition columns requires a new installation; generation admission
refuses to reinterpret existing rows under another scope.

## Group secrets

Kotlin's `KeyEpoch` exposes `generation` and `secretHash`, implements the writer
order and has a stable text representation. `GroupStatus.current` and each
`TableHolding.current` identify the winner; `epochs` and `keyEpochs` retain
complete identities. `share` accepts an optional complete identity.

One random secret per group partition and epoch identity derives every member
table's key ([Decision 0040](../decisions/0040-group-secrets.md)). Reading is
all-or-nothing per group partition; write rights stay per table. A table that
joins the group in a later generation derives its key from the held secret.

An identity contains the numeric generation and a hash of the group secret,
shared by every table. Concurrent creation produces first-generation siblings;
concurrent rotation produces siblings of the next generation. All remain
readable, while every member writes under the highest generation's lowest hash.
HKDF continues to use the numeric generation. The SDK saves the group record
before installing keys and resumes all member tables at reopen, including an
installation interrupted between tables. Siblings count against the 256 retained
group epochs. [Decision 0050](../decisions/0050-concurrent-key-epochs.md) defines
the order, formats and revocation metadata.

`RockyClient.partitions` names a group partition by group and slot values, in
`partitionBy` order; `address` and `complete` name one table. JSON requests use
the same names prefixed with `partition_`; values use the tagged SDK cells. The
native request may name `table` instead of `group` to mean that table's group.

| Operation | Effect |
| --- | --- |
| `create(group, values)` | Issuer: prepares the secret at the partition's current epoch, else epoch 1 |
| `share(group, values, recipients)` | Whole-group reader: saves one envelope of the current epoch per recipient node key, which sync delivers; returns no envelope |
| `grant` / `delegate` | Selector grants; an issuer records the device and user first |
| `register(group, values)` | Registers the addresses without a key, as custody; a custody peer also registers partitions devices offer ([0045](../decisions/0045-custody-partition-offers.md)) |
| `subscribe` / `bootstrap(group, values, omit)` | Holds every member except those left out; `bootstrap` saves checkpoint-first intent, joining one still pending |
| `unsubscribe` / `evict(group, values, keep)` | Releases every member except those kept |
| `status(group, values)` | Held sibling identities, the current winner, each member's identities, winner and holding, and on the sharer each recipient's envelope as `pending`, `deposited` or `delivered` |

The SDK verifies the publisher signature or the member signer's node chain
and whole-group read, and stores only the secret in
the installation's external key provider. `share` refuses a revoked recipient,
a non-current epoch and an unfinished revocation. A grant never delivers a key;
an envelope never grants authority. A recipient installs an envelope only
after it opens and the device reads every member in whole. A recipient without B's grant cannot
request B's range or checkpoint, register its key or admit its frames. A key
reveals a whole partition, including earlier ciphertext under that epoch.

A member's wildcard partition grant does not request every group key during
pairing or renewal. Members receive those keys through explicit sharing and
rotation delivery. A finite partition selector can receive its selected keys;
issuers receive every readable group through renewal.

**Delivery.** An envelope reaches its recipient alone. Each sync's first
session hands the peer the envelopes addressed to it and collects those the
peer holds for this device; the recipient returns a receipt signed by its node
key and the sender drops the envelope. A sender also deposits an envelope with
up to three mailboxes it meets: issuers and any peer with a custody grant.
Reader and writer profiles also carry custody; a raw read-only grant does
not. A mailbox serves a deposit only to the session of its recipient, hands
the receipt back to the depositor and forgets both; it holds 256 deposits per
recipient and 4,096 in all, each for 30 days, and refuses beyond as the
retryable `mailbox_full`. It learns that a recipient was sent something, when
and how large, and no partition: the group, the epoch and the secret are
sealed to the recipient. No replicated row records who holds which partition.
An epoch received twice installs once. A revoked recipient's envelopes are
dropped by the sender and by every mailbox that learns the statement.
[Decision 0054](../decisions/0054-envelope-delivery.md) defines the formats.

**Sharing with an enrolled device.** A profile reads every partition of its
tables, whether paired or supplied through `grant`, so one `share` opens a
list to it once it syncs with the sharer or a mailbox. A partitioned table's
own scope holds no rows: it offers no checkpoint, and a join's bootstrap skips
it. A shared list this device does not hold yet starts from a peer's
checkpoint after its join's bootstrap, so it needs no earlier epoch or revoked
author; the SDK records that intent as it installs the envelope. A device that
holds the list only installs the new epoch. The
[shopping example](../../examples/shoppinglist-app/README.md) shares this way.

**A partition the peer has not seen.** Two issuers may each create a
partition before they next meet. A replica learns a partition's tuple by
installing its envelope or from a custody offer; until then it cannot resolve
the peer's grant for it, so it refuses that member of the frontier batch.
Only that member is skipped: the session goes on for every other scope, and the
partition is neither pulled nor pushed with that peer.
Range, push and checkpoint requests for it stay refused, so the replica never
holds or serves its ciphertext. The SDK records `scopes_refused`; once the
partition is shared and installed, the next sync exchanges it
([decision 0046](../decisions/0046-per-scope-frontier-refusal.md)).

**Revocation.** One saved intent rotates: each group partition the device could
read gets one new secret, so its members advance to one epoch together, also
after an interruption. Affected are partitions whose members its enrollment
profile reads or a recorded grant selects, whichever issuer enrolled it.
A partition-only record selects only its grants. An absent record, including
an unrecorded delegate, rotates every known partition whose secret the
revoking issuer holds. A recorded user subject is revoked with its device,
so a fresh node key under that user cannot renew. Earlier epochs remain for retained history. The
completed rotation seals each new epoch to every unrevoked device whose
registry record reads the partition, saved with the rotation, and sync
delivers it; no application call shares it. A grant delegated on a non-issuer is not
recorded and uses that conservative rotation when its recipient is revoked;
no delegator is inferred. Revoking a delegator does not enumerate its
descendants or rotate extra scopes on their behalf, but shared chain ancestors
can end their credentials. Durable provenance is [deferred](../ideas/delegation_provenance.md).

**Holding rule.** A group partition is held whole unless the application leaves
a table out by name, for example attachments on a phone. Leaving out a table
that a held member references is refused, as is keeping a member without one
it references. `role(partial = true, fullNodes = ..., custody = ...)` selects
the partial role; eviction requires the existing full-node custody evidence and
keeps permanent replay/identity marks. Reopen reloads addresses, secrets,
derived keys and subscriptions.

## IDs and transactions

The numbered node credential signs partitioned inserts under the node key.
`nextId(table, partitionValues)` and SQL
`SELECT rocky_next_id('items', ?)` reserve the next ID in the active transaction.
With multiple partition columns, supply one argument per column. Rollback
releases reservations; commits continue the permanent per-author/partition
creation summary. Equal IDs in different partitions are valid.

A table partitioned by its own `id` counts per author/table. Call
`partitions.next("lists")` before opening the write transaction. It prepares the
group secret for the prospective log-derived ID and returns that ID and scope. Repeating
preparation before an insert is idempotent; it does not allocate a counter.
Inside the transaction, `nextId("lists")` reserves and verifies the same ID.
Use the prepared list ID to prepare its items partition before that transaction
as well. A concurrent commit may invalidate the candidate; retry preparation.

Trusted `SqlSession::next_id_partition` uses the same per-partition counters.
Trusted typed ID helpers require a partition when a table has duplicate IDs:
`update_partition_id`/`delete_partition_id`; an ambiguous unqualified ID errors.
SQL updates already identify the row by its complete business key.

## Proving holdings during sync

Possessing a profile grant alone reveals no partition holdings through a native
session. Clients prove a named scope with up to four session-bound values derived
from retained table keys. Readers accept any retained proved epoch, including
losing siblings and superseded generations. Keyless custody instead requires the
tuple and a whole-scope grant. A former reader keeps the tuple and a guessable
tuple proves little; custody cannot verify secret possession.

A partition without a matching proof is indistinguishable from an unknown scope;
it is omitted from scope enumeration. Proofs stay in session memory. Group secrets,
envelopes and epoch identities retain their formats. See
[proving a partition](replica-roles.md#proving-a-partition) and
[decision 0052](../decisions/0052-holding-proofs.md).

## SQL and completeness

A table is complete when a verified grant covers every schema partition column
with `Any`, grants whole-row read in its domain, and the replica has the full
role with every registered partition held, keyed and applied. A restricted SQL
session is partial even on that device. Missing keys, pending frames,
unsubscription and eviction prevent a completeness claim. This concerns held
coverage; ordinary peer lag still requires synchronization.

For a partly held table, every resolved reference must constrain all partition
columns by equality or finite `IN` to held values, or request a partial result
with `row_kind = 0`. Values and bound parameters are checked at execution.

```sql
SELECT * FROM items WHERE list = ?;              -- held list, or error
SELECT SUM(amount) FROM items;                   -- requires complete table
SELECT SUM(amount) FROM items WHERE row_kind=0;   -- total of held real rows
SELECT SUM(amount) FROM partial_items;           -- the application's view
```

The checker expands views, including views on views. It propagates equality
through constrained inner joins. In a `LEFT JOIN`, its right reference may use
an `ON` proof; an `ON` constraint cannot prove the preserved left side complete.
`OR` provides no proof. Reserved row-kind comparisons are refused. Reads wrap
partial references in held-tuple filters, preserving outer joins; updates and
deletes apply the same proof and filter to cached rows.

Unsupported proof forms, such as inequalities, functions or outer constraints
on a derived query lacking its own proof, fail conservatively. Proof tuple
products are capped at 4,096 and view expansion at 16 passes. Views must supply
one static SELECT and cannot introduce arbitrary SQL side effects. The existing
SQL authorizer continues to deny protocol tables, DDL and unrelated domains.

## Formats

| Item | Rule |
| --- | --- |
| Partition tuple | Sorted, nonempty map from column commitment to canonical key component, by the business-key codec (affinity, collation). Nulls, duplicates and tuples over 1 KiB are refused |
| Scope | Encryption commitment over domain, table and tuple under `rocky.partition-scope.v1`; the table commitment is unchanged |
| Definition/descriptor | Definition format 6, descriptor format 5, including partition groups, optional `partition_by` and views; others are refused before installation writes |
| Group address | Domain, group name and normalized slot values in declaration order under `rocky.partition-group.v1`; at most 1 KiB |
| Group secret envelope | Version 2, one secret per recipient, bound to manifest, group address, epoch identity and revocation IDs ([0050](../decisions/0050-concurrent-key-epochs.md)) |
| Frame | Row format 11 (`Extensions.row_format`) with the tuple in the signed header and the encryption associated data |
| Checkpoint | Authenticates the tuple and keeps per-scope identity marks; format 15 names its seal and optional partition ([encrypted checkpoints](encrypted-checkpoints.md)) |
| Bundle | Body version 3 ([row moves](row-moves.md)); each opening commits to its frame header and tuple |

Readers recompute the tuple from both row images and refuse a mismatch before
projection. Custodians verify the signed scope and partition grant without
opening the row. Author sequence marks and checkpoints are keyed by the
derived scope; importing or rebuilding one partition keeps the others. A table
partitioned by its own `id` allocates sequences per author and table and keeps
the maximum in the table's permanent creation summary.

## Limits and budget

| Limit | Value |
| --- | --- |
| Partition columns | 16 |
| Encoded tuple | 1 KiB |
| Selector values per column / total bytes | 256 / 16 KiB |
| Envelope recipients per call / size | 256 / 4 KiB |
| Secret epochs per group partition / recorded grants per holder | 256 / 256 |
| Registered partitions per replica | 4,096 |
| SQL proof tuple products / view expansion passes | 4,096 / 16 |

Every held table partition has its own derived key, logs and checkpoint. The 200-partition
measurement (`cargo test -p rocky-secure --lib measure_200_partitions --locked
-- --ignored --nocapture`, debug build) set these review budgets:

| Cost for 200 partitions | Measured | Budget |
| --- | ---: | ---: |
| Register keys and author 200 rows | 9.0 s | 15 s |
| Authenticated range catch-up | 11.4 s | 25 s |
| 200 checkpoints | 4.3 s | 12 s |
| Reopen with 200 key installations | 1.1 s | 5 s |
| Empty per-partition metadata | 389 B | 2 KiB |

The [phase 4 qualification](../features_06/04-partition-aware-constraints.md#qualification-on-the-assigned-base)
reran this probe on 2026-10-09: initialization 7.616 s, catch-up 14.562 s,
checkpoints 5.285 s, reopen 1.068 s and metadata 409.6 B per partition. All review
budgets passed. This is a single-table probe. The [phase 9 group-sync measurement](../features_06/09-group-sync-measurement.md)
compares one and four ERP tables, including authenticated idle sync. The SDK
exchanges group-member frontiers together; keys, logs and checkpoints stay
per table scope ([Decision 0044](../decisions/0044-group-frontier-exchange.md)).

### Four-table profile

[Phase 3](../features_07/03-catch-up-cost.md) profiled the four-table fixture
(`measure_200_partition_group_sync`) on 2026-10-10: macOS arm64, debug build, a
host shared with other builds. `ROCKY_PROFILE_DIR=<dir>` makes the fixture sample
each stage with macOS `sample`; a sample counts toward the outermost named cost
on its stack, and estimated seconds scale a share to the unsampled stage.
The [call trees, stage logs and analyzer](../../benchmarks/runtime-scaling/evidence/catch-up-2026-10-10/README.md)
retain the evidence. Named costs cover 91.3%, 98.8% and 98.0% before, and
84.8%, 92.5% and 90.0% after. Rounded rows may not add to 100%.

| Four tables, 200 partitions | Budget | Before | After |
| --- | ---: | ---: | ---: |
| Register keys and author | 60 s | 45.5 s (86 s in phase 9's run) | 18.4 s |
| Authenticated range catch-up | 100 s | 177.9 s | 49.7 s |
| 801 checkpoints | 48 s | 49.9 s | 8.6 s |

The continuation rerun after local heavy builds finished took 32.910 s,
84.143 s and 10.105 s for four tables; all three budgets passed. Its log and
a prior contended run that missed two budgets are kept in the evidence. The
final build, including receipt-retry recovery, took 28.507 s, 68.200 s and
8.911 s; all three budgets passed again.

One table in the profiled after run takes 2.5 s, 6.3 s and 1.1 s. Before,
by share of each stage:

| Cost | Register and author | Catch-up | Checkpoints |
| --- | ---: | ---: | ---: |
| This device's chain resolved against every registered partition, per frame, scope and step | 52% | 29% | 75% |
| Peer and frame chains verified, per request and frame | 6% | 12% | |
| CBOR of frame headers, 94% of whose bytes are chains | 20% | 13% | 9% |
| CBOR of the session's signed bytes: binding and message as integer arrays | | 18% | |
| CBOR of checkpoint records, 87% of whose bytes are chains, including signing | | | 7% |
| CBOR of payloads | 3% | 2% | 3% |
| Bundle members trial-decrypted with every held key | | 8% | |
| Other sealing, signatures and hashing | 3% | 3% | 2% |
| SQLite and device flushes | 6% | 13% | 1% |
| Unattributed | 9% | 1% | 2% |

The fixes need no format change. The device's grants are resolved once per
authority, partition count and second; `validate_keys` checks a set; frames
decoded recently are kept by a digest of their bytes; a session encodes the
start of its signed bytes once and the message by byte; bundle members are
tried with their own and their partition's keys first. Every request still
verifies its chains against the current revocations.

After, the costs left are, by share of each stage:

| Cost | Register and author | Catch-up | Checkpoints |
| --- | ---: | ---: | ---: |
| This device's chain, once per new partition | 38% | 1% | 2% |
| Peer chains verified per request | | 38% | |
| Frame chains verified per frame and authoring | 12% | 10% | |
| Chain bytes: CBOR of headers and checkpoint records, signing over their chains | 10% | 8% | 48% |
| CBOR of payloads and other values | 10% | 11% | 24% |
| Device flushes | 4% | 11% | |
| Other sealing, signatures, hashing, SQLite and named chain work | 11% | 12% | 17% |
| Unattributed | 15% | 8% | 10% |

The chain-byte cost is an estimate from encoded-type samples and byte shares,
not a direct timer: headers count at 94%, checkpoint records at 87%, and
session messages at the 61% of frame bytes that are chains. This gives 1.8 s, 4.2 s and 4.1 s of the three stages. The 801 frames of the
catch-up carry 7.27 MB of headers, 6.80 MB of them chains, beside 3.87 MB of
payload; the 801 checkpoint records hold 11.31 MB, 9.82 MB of them chains.
[Phase 11](../features_07/11-chains-by-reference.md) started from these figures.

Chain bytes were 48% of the checkpoint stage, above the 20% at which phase 11
moves the chain out of the frame header as well as off the checkpoint page
([decision 0053](../decisions/0053-chains-by-reference.md)). Rerun on
2026-10-10 on the same kind of host, under another load:

| Four tables, 200 partitions | Budget | Phase 3 | Phase 11 |
| --- | ---: | ---: | ---: |
| Register keys and author | 60 s | 18.4 s | 4.4 s |
| Authenticated range catch-up | 100 s | 49.7 s | 18.5 s |
| 801 checkpoints | 48 s | 8.6 s | 1.5 s |
| Headers of the 801 catch-up frames | | 7.27 MB | 0.59 MB |
| Chains of those frames | | 6.80 MB, in every header | 41.9 KB, stored once |
| 801 checkpoint records / their signers' chains | | 11.31 / 9.82 MB | 11.43 / 9.87 MB |

All three budgets are met. The single-table probe took 1.5 s, 1.5 s, 0.3 s
and 0.1 s to reopen and passes its budgets above. A signed checkpoint still
embeds its signer's chain. The
[logs and gate measurements](../../benchmarks/runtime-scaling/evidence/chains-2026-10-10/README.md)
are kept.

## Design rationale

- A subscription selects scopes and cannot narrow a key, so confidentiality
  per partition needs a key per partition, and with it per-partition logs and
  checkpoints.
- The tuple uses the business-key codec so SQL-equal values, including
  NOCASE/RTRIM, select one partition.
- An unpartitioned scope keeps its identity; a partitioned descriptor or frame
  is never read as an unpartitioned one, and partition metadata never changes
  in place.
- Completeness is a property of held grants, keys and applied partitions; the
  credential primitives alone do not establish it.
