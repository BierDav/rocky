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
group namespace. Omitting `group` creates a private group named by the table's
logical identity; another table cannot name that group. An unpartitioned table
cannot join. Group declarations affect canonical bytes and replicated identity.

`LogicalSchema.partition_groups()` exposes groups, ordered slots and member
tables to native runtime/SDK consumers. `group_address(domain, table, values)`
returns the same address across members for SQL-equal positional values,
including NOCASE and RTRIM spellings, independent of their column identities.
The address binds the domain and group name in slot declaration order.
[Decision 0037](../decisions/0037-partition-groups.md) pins its encoding.

Table scopes, logs, checkpoints, grants and pseudonyms stay per table; the key
comes from the [group secret](#group-secrets). Native partition-complete
`LatestWins` indexes resolve per partition;
[mergeable constraints](mergeable-constraints.md) describes their identities and
rebuild boundary. In-group references and references to an unpartitioned
logged parent are admitted. Partitioned `SetNull` indexes, references across
groups, existence logs and command control remain refused, by the Kotlin builder
and Room import as natively.

Changing an existing table's group or renaming a group is refused by generation
admission; a new table may join an existing group. Removing a table remains
refused. Changing partition columns requires a new installation; generation admission
refuses to reinterpret existing rows under another scope.

## Group secrets

One random secret per group partition and key epoch derives every member
table's key ([Decision 0040](../decisions/0040-group-secrets.md)). Reading is
all-or-nothing per group partition; write rights stay per table. A table that
joins the group in a later generation derives its key from the held secret.

`RockyClient.partitions` names a group partition by group and slot values, in
`partitionBy` order; `address` and `complete` name one table. JSON requests use
the same names prefixed with `partition_`; values use the tagged SDK cells. The
native request may name `table` instead of `group` to mean that table's group.

| Operation | Effect |
| --- | --- |
| `create(group, values)` | Issuer: prepares the secret at the partition's current epoch, else the policy epoch |
| `share(group, values, recipients)` | Issuer: one envelope of the current epoch per recipient node key |
| `install(group, values, envelope)` | Opens the envelope, then requires whole-partition read on every member before anything is stored |
| `grant` / `delegate` | Selector grants; an issuer records the device and user first |
| `register(group, values)` | Registers the addresses without a key, as custody; a custody peer also registers partitions devices offer ([0045](../decisions/0045-custody-partition-offers.md)) |
| `subscribe` / `bootstrap(group, values, omit)` | Holds every member except those left out; `bootstrap` saves checkpoint-first intent, joining one still pending |
| `unsubscribe` / `evict(group, values, keep)` | Releases every member except those kept |
| `status(group, values)` | Held secret epochs, and each member's key epochs and holding |

The SDK pins its publisher as delivering issuer and stores only the secret in
the installation's external key provider. `share` refuses a revoked recipient,
a non-current epoch and an unfinished revocation. A grant never delivers a key;
an envelope never grants authority. A recipient without B's grant cannot
request B's range or checkpoint, register its key or admit its frames. A key
reveals a whole partition, including earlier ciphertext under that epoch.

**Sharing with an enrolled device.** A profile reads every partition of its
tables, whether paired or supplied through `grant`, so one `share` and one
`install` open a list to it. A partitioned table's own scope holds no rows: it
offers no checkpoint, and a join's bootstrap skips it. A device that does not
hold the list yet calls `bootstrap` after its join's bootstrap has finished, so
it starts from a peer's checkpoint and needs no earlier epoch or revoked
author; a device that holds it only installs the new epoch. The
[shopping example](../../examples/shoppinglist-app/README.md) shares this way.

**A partition the peer has not seen.** Two issuers may each create a
partition before they next meet. A replica learns a partition's tuple by
installing its envelope or from a custody offer; until then it cannot resolve
the peer's grant for it, so it refuses that member of the frontier batch.
Only that member is skipped: the session goes on for every other scope, and the
partition is neither pulled, pushed nor horizon-acknowledged with that peer.
Range, push and checkpoint requests for it stay refused, so the replica never
holds or serves its ciphertext. The SDK records `scopes_refused`; once the
partition is shared and installed, the next sync exchanges it
([decision 0046](../decisions/0046-per-scope-frontier-refusal.md)).

**Revocation.** One saved intent rotates: each group partition the device could
read gets one new secret, so its members advance to one epoch together, also
after an interruption. Affected are partitions whose members its enrollment
profile reads or a recorded grant selects; every known partition when the
issuer never enrolled the device. Its user subject is revoked with it, so a
fresh node key cannot renew. Earlier epochs remain for retained history; share
the new epoch to remaining readers. A grant delegated on a non-issuer is not
recorded and is covered only by that conservative rotation.

**Holding rule.** A group partition is held whole unless the application leaves
a table out by name, for example attachments on a phone. Leaving out a table
that a held member references is refused, as is keeping a member without one
it references. `role(partial = true, fullNodes = ..., custody = ...)` selects
the partial role; eviction requires the existing full-node custody evidence and
keeps permanent replay/identity marks. Reopen reloads addresses, secrets,
derived keys and subscriptions.

## IDs and transactions

Install the table pseudonym through the credential APIs before inserting.
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
| Definition/descriptor | Format 4, including partition groups, optional `partition_by` and views; others are refused before installation writes |
| Group address | Domain, group name and normalized slot values in declaration order under `rocky.partition-group.v1`; at most 1 KiB |
| Group secret envelope | Version 1, one secret per recipient, bound to manifest, group address and epoch ([0040](../decisions/0040-group-secrets.md)) |
| Frame | Row format 6 (`Extensions.row_format`) with the tuple in the signed header and the encryption associated data |
| Checkpoint | Authenticates the tuple and keeps per-scope identity marks; format 6 has independent optional partition and horizon fields ([encrypted checkpoints](encrypted-checkpoints.md)) |
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
