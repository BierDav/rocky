# Partitioned scopes

A table's partition columns are a confidentiality boundary inside its domain:
each partition has its own key, logs and checkpoints, because a subscription
cannot narrow what one key reveals. This guide is for an application using
schema declarations and the packaged SDK;
[partition credentials](partition-credentials.md) defines selectors and envelopes.

## Declare the boundary

```kotlin
val shopping = schema("shopping") {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id") }
    table("items") {
        integer("list"); text("key"); integer("amount")
        primaryKey("list", "key"); partitionBy("list")
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
    table("items") { partitionBy("list") }
}
```

Changing partition columns requires a new installation; generation admission
refuses to reinterpret existing rows under another scope.

## Keys, credentials and subscriptions

`RockyClient.partitions` exposes address, grant, delegate, create, register,
install, envelopes, role, subscribe, unsubscribe, evict and complete operations.
All values use the existing tagged SDK cells, in `partitionBy` declaration order.
JSON requests use the same operation names prefixed with `partition_` and go
through the native bindings; no Rust source is required.

The issuer calls `create(table, values, epoch)` to persist a concrete address and
its random key. `address` returns the canonical tuple and scope, from which an
application builds a selector grant. `grant` issues it to a prepared device;
`delegate` narrows an existing delegable chain. Selector grants carry domain,
table, selector, row selection, rights and fields together. A reader may receive
only A while an ERP grant selects one customer and any warehouse.

The issuer calls `envelopes(table, values, recipients, epoch)` for independent
recipient-bound envelopes. The reader calls `install` with its envelope and the
same address/epoch. The SDK pins its publisher as delivering issuer, verifies
whole-partition read authority, saves the key in the external installation Vault
and records metadata before runtime installation. A custody node uses `register`
without a key. Valid incoming frames can register an authorized address too.
The frame signature, tuple/schema match and custody grant are checked first.

A grant never delivers a key; an envelope never grants authority. A recipient
without B's grant cannot request B's range or checkpoint, register its key, or
admit its frames. Keys reveal a whole partition, including earlier ciphertext
under that epoch. Revocation cannot make a previously delivered secret unknown.

Revocation saves intent before rotating known partition keys conservatively,
revokes the device and its assigned pseudonyms, renews the issuer's own authors,
and installs new epochs. Earlier keys remain for retained history. Issue fresh
credentials and envelopes to remaining readers. Supplied credential renewal
accepts selectors; narrowing removes runtime readability and unauthorized keys
are not loaded on reopen. Partition envelope delivery is explicit application
plumbing, as are grants and choice of recipients.

`role(partial = true, fullNodes = ..., custody = ...)` selects the partial role.
Subscribe/register addresses before catching up. Unsubscribe stops following a
scope; evict requires the existing full-node custody evidence, removes its rows
and operations, and keeps permanent replay/identity marks. Other partitions'
projection rows remain. Reopen reloads durable addresses, keys and subscriptions.
Epoch retirement uses the existing per-scope retained-history checks.

## IDs and transactions

Install the table pseudonym through the credential APIs before inserting.
`nextId(table, partitionValues)` and SQL
`SELECT rocky_next_id('items', ?)` reserve the next ID in the active transaction.
With multiple partition columns, supply one argument per column. Rollback
releases reservations; commits continue the permanent per-author/partition
creation summary. Equal IDs in different partitions are valid.

A table partitioned by its own `id` counts per author/table. Call
`partitions.next("lists")` before opening the write transaction. It prepares the
key for the prospective log-derived ID and returns that ID and scope. Repeating
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
| Definition/descriptor | Format 3 with `partition_by` or views, otherwise 2; others are refused before installation writes |
| Frame | Row format 5 (`Extensions.row_format`) with the tuple in the signed header and the encryption associated data |
| Checkpoint | Authenticates the tuple and keeps per-scope identity marks; its format number depends on content ([encrypted checkpoints](encrypted-checkpoints.md)) |
| Bundle | Body unchanged; each opening commits to its frame header and tuple |

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
| Registered partitions per replica | 4,096 |
| SQL proof tuple products / view expansion passes | 4,096 / 16 |

Every held partition has its own key, logs and checkpoint. The 200-partition
measurement (`cargo test -p rocky-secure --lib measure_200_partitions --locked
-- --ignored --nocapture`, debug build) set these review budgets:

| Cost for 200 partitions | Measured | Budget |
| --- | ---: | ---: |
| Register keys and author 200 rows | 9.0 s | 15 s |
| Authenticated range catch-up | 11.4 s | 25 s |
| 200 checkpoints | 4.3 s | 12 s |
| Reopen with 200 key installations | 1.1 s | 5 s |
| Empty per-partition metadata | 389 B | 2 KiB |

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
