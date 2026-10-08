# features_05: agreed scope

Status: agreed on 2026-10-07; every open question is decided in
[the decisions](#decisions-of-2026-10-07). Implementation status lives in the phase briefs. The
work is sequenced in [the README](README.md), which is the
next-work authority; this document is the design they implement.

Every item is **Agreed**: decided in the design discussions of 2026-10-07.

Statements about current behaviour come from the linked guides and source.
The designs are reasoning from those; none was prototyped.

## Goal

Make Rocky sufficient for a first real shopping-list application, and lay
foundations that carry to an ERP: a real ORM integration, relational
constraints that survive offline merging, access control below the table
level, and bounded history. The library stays modular and unopinionated: it
enforces what credentials say and offers cryptographic primitives; it does not
decide how an application models permissions.

## Summary

| Area | Result | Status |
| --- | --- | --- |
| [SQLite driver and Room](#1-sqlite-driver-and-room) | Room DAOs read and write Rocky's own file; one Room transaction is one atomic bundle | Agreed |
| [Schema declaration](#2-schema-declaration) | Kotlin builder with compound keys; Room import fills the same builder | Agreed |
| [Row identity](#3-row-identity) | Every table carries a verifiable `id` and a `row_kind`; a deleted key can be inserted again | Agreed |
| [Partitioned scopes](#4-partitioned-scopes) | Row-level read scopes by partition columns; explicit reads of partly held tables | Agreed |
| [Constraints in mergeable tables](#5-constraints-in-mergeable-tables) | Real unique indexes and foreign keys, also across scopes, with puppet parents | Agreed |
| [Bootstrapping and the horizon](#6-bootstrapping-and-the-horizon) | Checkpoint-based join and catch-up; strong history pruned behind the checkpoint; tombstones and hidden rows dropped at an agreed horizon | Agreed |
| [Credential primitives](#7-credential-primitives) | Raw grants, delegation, partition selectors, key envelopes, author pseudonyms | Agreed |
| [Strong commands](#8-strong-commands) | Keyed handler reads now; retained history bounded by section 6, slice C; read-only Room access to confirmed rows | Agreed |
| [Platform and operations](#9-platform-and-operations) | Android, an always-on peer, issuer recovery | Agreed |
| [Not in scope](#10-not-in-scope) | Own query language, intent capture, large command tables, optimistic updates, iOS | Agreed |

## 1. SQLite driver and Room

**Agreed.**

### Problem

The current [Room adapter](../runtime/room-integration.md) keeps Room in a
second file. Rocky's rows are copied into it by whole-table replacement, capped
at 100,000 rows, and writes go through `RoomTable` and a durable queue. Room
and Rocky commit separately, so there is no transactional consistency, and
`@Insert`, `@Update` and `@Delete` are local cache changes that never
replicate.

### Decision

Room has no database of its own. Its connections open Rocky's file through a
Rocky implementation of `androidx.sqlite.SQLiteDriver`.

- A Room transaction is one Rocky commit and one signed
  [bundle](../runtime/secure-sessions.md#bundles). Peers apply it atomically.
  Nested Room transactions map to savepoints.
- Capture stays in the Rust engine, below the driver, through SQLite's
  pre-update hook. An unsupported driver call is an error; it can never become
  a silent unreplicated write. No customised SQLite build is needed.
- The typed tables Room sees carry **real** primary keys, indexes, unique
  indexes and foreign keys ([section 5](#5-constraints-in-mergeable-tables)),
  so the file is readable with ordinary SQLite tooling.

### Work

| Where | Change |
| --- | --- |
| `sdks/native` | Typed statement, cursor and table-event handles. The JSON `call` per step is too slow for a driver |
| New `sdks/kotlin/rocky-sqlite` | The `SQLiteDriver`, without a Room dependency, so other ORMs on that interface can use it. Writer connection: a SQL session. Reader connections: [snapshot cursors](../runtime/queries-and-observation.md#snapshot-cursors) |
| `sdks/kotlin/rocky-room` | Becomes thin: schema import, Room's housekeeping statements, invalidation |
| Shopping example | Moves to plain DAOs |

Room's housekeeping statements (`PRAGMA user_version`, `table_info`,
`index_list`, `room_master_table`, `CREATE TABLE IF NOT EXISTS`) are all
`Denied` today. They are answered from the session
[catalog](../runtime/queries-and-observation.md#catalogs) for a pinned Room
version. Room's invalidation tracker relies on triggers on its own connection,
which sync writes never fire; the driver emulates its modification log from
Rocky [table events](../runtime/queries-and-observation.md#table-events)
instead. The [pinned Room findings](../old/features_03/research.md#exact-room-version-inspected)
and the old Room recording fixtures are the acceptance reference.

The projection adapter (`RoomReplica`, `PendingMutation`, `ReplicaMetadata`)
is removed once the driver qualifies. Two maintained paths are not kept.

### Limits an application author meets

These fail at setup or at the statement, with a clear error:

- `autoGenerate = true` keys are refused. Every table carries an `id` that
  Rocky issues ([section 3](#3-row-identity)).
- Every entity declares the `id` and `row_kind` columns.
- A read of a table the device holds only partly must name a held partition
  or carry `row_kind = 0`
  ([section 4](#queries-on-a-partially-held-table)).
- A write transaction holds the whole replica and times out after 5 s by
  default ([secure SQL](../runtime/secure-sql.md#leases-and-limits)).
- A bundle is capped at 2,048 rows and 4 MiB. A larger transaction fails; it
  is not split.
- A bundle records the net change. A row inserted and deleted in one
  transaction publishes nothing.
- `SET n = n + 1` publishes the computed value, not an increment
  ([section 10](#10-not-in-scope)).

A whole-entity `@Update` is safe: under the `changed` policy only the columns
that differ are published.

## 2. Schema declaration

**Agreed.**

The Kotlin `Schema` cannot declare a key today; the SDK generates one
`id TEXT PRIMARY KEY` per table. The core already supports compound primary
keys and builds real `PRIMARY KEY` clauses from
[definition data](../runtime/schema-definitions.md).

- The Kotlin builder is the primary path. It becomes a thin builder for
  definition data and gains: compound primary keys, non-unique indexes, `real`
  and `blob` columns now; unique indexes, foreign keys, partition columns and
  conflict policies with sections 4 and 5.
- Every table gets the `id` and `row_kind` columns of
  [section 3](#3-row-identity). They are not declared. A Room entity must
  carry both as fields.
- Room import fills the same builder from Room's exported schema, so tables
  and keys are not declared twice. Rocky-only attributes, which Room's export
  cannot express, are added on top in Kotlin. This replaces the separate
  policy JSON of [logical schemas](../runtime/logical-schema.md#generate-a-schema).
- Both produce one descriptor. Nothing downstream distinguishes them, and no
  path needs Rust.

Illustrative shape, not an implemented API:

```kotlin
val Shopping = schema("shopping", version = 1) {
    // `id` and `row_kind` exist on every table and are not declared.
    val lists = table("lists") {
        text("title")
        primaryKey("id")
        partitionBy("id")
    }
    table("items") {
        text("title"); boolean("done")
        reference("list", lists, onDelete = Delete.Cascade)
        primaryKey("list", "id")
        partitionBy("list")
        unique("list", "title", onConflict = Conflict.SetNull)
    }
}
// With Room: schema.fromRoom(exportedJson) { table("items") { partitionBy("list") } }
```

Named roles are deliberately absent; see [section 7](#7-credential-primitives).

## 3. Row identity

**Agreed.**

### Problem

The secure profile identifies a row by table and key alone, so a deleted key
cannot be inserted again. An N:M link table with `PRIMARY KEY(a, b)` could not
unlink and relink the same pair. This is a gap features_04 deferred, not a
protocol limit. Ordering by stamp alone is not enough: an offline edit made
against the old row would land on the new one, because both share one
identity.

The trusted profile solves this with incarnations
([decision 0015](../decisions/0015-logical-schema-and-row-identity.md#incarnations)).
Here the incarnation becomes a visible column, so that a foreign key value
names exactly one incarnation of its parent
([section 5](#foreign-keys)).

### Two columns on every table

| Column | Type | Content |
| --- | --- | --- |
| `id` | 64-bit integer | The identity of this incarnation of the row. Issued by Rocky, never updated |
| `row_kind` | integer | 0 for a real row, 1 for a puppet, 2 for the virtual row ([section 5](#puppets)) |

Both exist on every table and on every node, so the schema of a partial, full
and trusted node is the same. An application only ever reads `row_kind = 0`.

A partitioned table needs a compound primary key: the partition columns plus
`id`, or a natural key together with a unique index on the partition columns
plus `id`. That index is the table's **identity index**.

### The `id`

The `id` is a snowflake: an author and a sequence in one integer. Unlike the
original scheme it has no timestamp and does not sort by time; a timestamp
would reveal when and how much a device works.

| Bits | Field | Content |
| --- | --- | --- |
| 1 | Zero | Keeps the value a positive `Long` |
| 31 | Author number | Names a pseudonym: one device in one table ([section 7](#7-credential-primitives)) |
| 32 | Sequence | Counts the rows this author created in this partition, from 1, without gaps |

- An `id` is unique together with the partition columns, not across the table.
  One counter per table would let the gaps in one partition reveal an author's
  activity in the others.
- The device keeps no extra state. The next sequence is the highest one in its
  own log for that partition, plus one. An application can obtain an `id`
  inside a transaction before it inserts, so a parent and its children fit in
  one Room transaction.
- Author 0 is reserved. The virtual row of every table has `id = 0`.
- A table partitioned by its own `id`, such as `lists` in the example of
  section 2, has one row per partition. Its sequence counts per author and
  table instead.
- A transport that cannot carry 64-bit integers exactly, such as JSON read by
  JavaScript, carries the `id` as a string.

Uniqueness is verified by receivers, not trusted from the generator:

- The author number must belong to the key that signed the frame.
- The sequence must be higher than the highest one already seen from that
  author in that partition.

The highest sequence per author and partition is kept permanently; it is one
number each. An identity therefore cannot return after its tombstone was
dropped at the horizon ([section 6](#6-bootstrapping-and-the-horizon)).

### Key reuse

Each insert creates a new `id`. Updates and deletes name an `id`, so a stale
edit or an old tombstone never reaches a replacement. The visible row for a
natural key is the live `id` with the latest creation stamp. The identity used
by grants stays key-based.

| Situation | Result on every replica |
| --- | --- |
| Unlink, then relink the same pair | A new row with a new `id` |
| One device deletes while another inserts the key again | The inserted row survives |
| Late edit against the deleted row | No effect |

This is a new frame and checkpoint format and needs a decision record.

## 4. Partitioned scopes

**Agreed.** Per-list sharing in the shopping app is the first test; an ERP is
the target.

### Model

Today a reader scope is one table: a key reveals every row of it
([credentials limits](../runtime/credentials.md#limits)). A partitioned table
declares one or more partition columns. Each distinct tuple of their values is
its own scope with its own key.

- Partition columns are part of the primary key. They are therefore
  immutable; changing one is a delete and an insert, as any key change is.
- The partition tuple is part of what a frame authenticates. A peer cannot
  seal a row of customer A under customer B's key.
- Every reader checks that the author holds the insert, update or delete
  right in that partition, using the existing pairing of row selection and
  column set in a grant.
- A partition key can be wrapped to several recipients, so "readable by the
  project team or by the department" is expressible.
- Column privacy uses a companion table with its own scope, as today.

A custody-only node cannot check row contents, only the signature and the
grant on the scope. It never applies rows, so this is acceptable.

### Partial replicas

A device receives frames only of scopes it holds a read or custody grant for
([replica roles](../runtime/replica-roles.md)). Keeping a replica small is not
an access question and has two tools:

- **Subscribe to a subset of partitions.** Subscribe and evict per scope
  exist. Choosing partition columns with size in mind (fiscal year, warehouse)
  covers most cases and stays peer-to-peer and verifiable. In scope.
- **Filtered cache from a full node.** A device sends a predicate and a full
  node returns matching rows as state, building on remote reads. The device
  cannot verify completeness, serve peers from it or evaluate constraints
  locally. Not in features_05; see
  [arbitrary paging](../ideas/arbitrary_paging.md).

Filtering the operation log by predicate is not attempted: sync and
checkpoints rely on complete per-author logs per scope.

### Queries on a partially held table

**Agreed.** A table is fully replicated on a device when the device's grants
cover every partition of it and none is evicted. A device knows this about
itself; no domain-wide record of full replicas is needed. On any other device
a statement that reads the table must, for that table, do one of two things:

- **Name held partitions.** It constrains every partition column by equality
  or `IN`, directly or through a join to a constrained column. The values are
  checked at execution; a partition that is not held is an error.
- **Ask for a partial result.** It carries the condition `row_kind = 0`. The
  result is every real row the device holds.

Anything else is rejected at the statement. The same applies to the row
selection of `UPDATE` and `DELETE`. `row_kind = 1` or `2` is never accepted,
so [puppets](#puppets) cannot be read.

```sql
SELECT * FROM bookings WHERE month = ?;                -- held month, or an error
SELECT SUM(amount) FROM bookings;                       -- rejected
SELECT SUM(amount) FROM bookings WHERE row_kind = 0;    -- total of what is held
```

The same schema therefore gives the same answer or an error on a partial, full
or trusted node. A different answer is possible only where the statement asks
for it.

The recommended form is a view named `partial_<table>`, because the name tells
the reader that the result is incomplete:

```sql
CREATE VIEW partial_bookings AS SELECT * FROM bookings WHERE row_kind = 0;
```

- The application declares the view, in Room with `@DatabaseView`. Rocky
  creates none and reserves no name.
- The check uses the syntax tree the engine already builds for every
  statement. A statement that reads a view is checked against the view's body,
  also for a view built on another view.
- The condition must apply to the whole table reference; inside an `OR` it
  does not count. In a `LEFT JOIN` it belongs in the `ON` clause or in the
  view, or the join becomes an inner join.

### Known limits

- Arbitrary access predicates (`amount < 1000`) need a trusted host that
  filters what it sends. Not required.
- Totals over rows a device cannot read need a host that publishes derived
  tables.
- Revocation rotates keys; it cannot retract what was read.
- Each scope has its own checkpoint and stores rows twice. Thousands of
  partitions need that overhead reduced. No budget is fixed in advance: the
  first ERP-shaped test holds 200 partitions on one device, and its
  measurement sets the budget in that brief.

### Domain or scope

A domain is a trust and ordering boundary: its own issuer, membership, policy
epoch and command sequence. A scope is a confidentiality and sync boundary
inside one domain. A list as its own domain works with today's cryptography
but gives one Room database per list and no SQL across lists. Partitioned
scopes were chosen because the ERP target needs them anyway.

## 5. Constraints in mergeable tables

**Agreed.** A mergeable table is one any device may edit offline, merged per
column with the latest write winning. Unique indexes and foreign keys are
refused in such tables today, because two valid offline writes can violate
them together.

These rules give **convergence, not invariants**. Both authors saw their
write succeed before the merge. Anything that must never be violated, such as
document numbers, stock or postings, is a strong command.

### Mechanism

Rocky keeps every merged row in its private state and writes only the resolved
result into the typed table. The real index and foreign key in the typed
table therefore always hold. A row kept out of the typed table is **hidden**.

- On the authoring device the constraint is a real SQLite constraint: a local
  duplicate or dangling reference fails at once. A policy only decides what
  happens when a remote write collides.
- Hidden is a pure function of the merged rows. It is never written as an
  operation, and a hidden row returns when the cause goes away, for example
  when the winner changes its value. This keeps the result independent of
  arrival order.
- Hidden rows are invisible to the application. There is no conflict table.
- Hidden rows become final and are dropped at the agreed horizon, together
  with tombstones ([section 6](#6-bootstrapping-and-the-horizon)). Making a
  loss permanent immediately was considered and not wanted.

### Unique indexes

A unique index in a mergeable table must include the table's partition
columns, so everyone who can read one clashing row can read the other.
Uniqueness across partitions is a strong command. The later stamp wins.

| Policy | The loser |
| --- | --- |
| `SetNull` | Its indexed non-partition columns read NULL while the clash lasts; they must be nullable |
| `LatestWins` | The whole row is hidden, as a primary-key clash is today |

A unique index covers real rows only. It is a partial index on
`row_kind = 0`, so two [puppets](#puppets) with the same default value do not
clash.

### Foreign keys

A foreign key references the parent's identity index: the parent's partition
columns plus its `id` ([section 3](#3-row-identity)). The value therefore
names one incarnation of the parent and the partition it lives in. A reference
to a natural key, such as `currencies(code)`, is not offered; `code` becomes a
unique column and the reference uses the `id`.

Child and parent may lie in different scopes. This replaces the earlier
restriction to the same partition or a declared companion table.

The foreign-key columns merge as one unit, not per column, so two concurrent
changes cannot mix into a reference nobody wrote.

| Delete action | A child whose parent is not alive |
| --- | --- |
| `CASCADE` | Hidden, then dropped at the horizon |
| `SET NULL` | Its reference reads NULL, then is stored as NULL at the horizon; the columns must be nullable and outside the primary key |

`RESTRICT` is not offered, because a concurrent child insert would break it.
An application that wants the check queries for children before it deletes.

- A parent that is deleted and created again has a new `id`. Children of the
  old one stay orphans.
- The effect on children is derived on every device from the parent's
  tombstone. It is not published as operations. A parent delete therefore also
  removes children that the deleting device cannot read or may not delete.
- The effect becomes final only at the horizon
  ([section 6](#6-bootstrapping-and-the-horizon)). A later change that points
  the child at another parent may still arrive, and applying the delete at
  once would make the result depend on arrival order.
- A child that arrives before its parent is hidden until the parent does.
  Incoming bundles apply with deferred foreign-key checks.
- A device may insert a child, or point one at another parent, for any parent
  it can name: a row it holds, or a puppet that is alive. A reference to a
  parent that is not alive is an ordinary orphan on every device, so receivers
  need no extra check.

### Existence log

A device that holds a child but not its parent's scope must still learn that
the parent was deleted. Otherwise it would show children that others consider
gone, and no two devices could drop the same orphans.

In the shopping example a list and its items share one scope per list, so no
foreign key crosses a scope and `lists` needs no existence log. Cross-scope
keys, and the exposure below, are reserved for the ERP cases that need them.

Each partition of a table that is referenced from another scope therefore has
a second, small log beside its sealed one:

- One record per row, `created` or `deleted`, with the row's `id` and no
  column values.
- Every device of the domain can read it and can verify the signature and the
  author's insert or delete right, without the partition's key.
- The log alone decides whether a row is alive, also for devices that hold the
  partition. A writer cannot make parent holders and other devices disagree.
- A device follows the existence logs of the partitions its children
  reference.

What it reveals to every device of the domain, and never more:

- how many rows such a partition has and when they are created or deleted;
- which [pseudonym](#7-credential-primitives) created each of them.

Rules that follow:

- Installation lists the tables that have an existence log, so the exposure is
  a visible choice of the schema author.
- A table whose mere row count is sensitive is not referenced from another
  scope.
- External parties, such as suppliers or customers, belong in their own
  domain.
- A pseudonym is not anonymity. A colleague who knows who created one row can
  follow that author in that table;
  [rotating pseudonyms](../ideas/pseudonym_rotation.md) would limit this.

### Puppets

A device that holds a child but not the scope of its parent has no parent row,
and the real foreign key in the typed table would fail. It creates a
**puppet**: a row with the parent's key and `row_kind = 1`. The puppet reveals
nothing, since its key is what the child's reference already contains.

| Parent's scope | Parent row | Result |
| --- | --- | --- |
| Held | Alive | The real row |
| Held | Not alive | The child is an orphan, see the delete action |
| Not held | Alive in the existence log | A puppet |
| Not held | Not alive in the existence log | The child is an orphan |

- **Values.** A nullable column is NULL. A `NOT NULL` column holds the zero
  value of its type: empty text, an empty blob, `false`, `0`, `0.0`.
- **Its own references.** A puppet's `NOT NULL` foreign key to a parent it
  does not know points at that table's **virtual row**, one row per table with
  `id = 0` and `row_kind = 2`. The virtual row's own references point at the
  virtual row of the next table, or at itself. Where the unknown parent's
  identity is contained in the puppet's key, as with `items.list`, an ordinary
  puppet is used instead.
- **Never readable.** A read of the table must name a held partition, and
  puppets lie in partitions that are not held; a partial read carries
  `row_kind = 0` ([section 4](#queries-on-a-partially-held-table)). The
  parent's `id` is the child's own foreign-key column, so no join is needed.
- **Read-only.** A trigger rejects an update or delete of a puppet or virtual
  row. It is visible in the schema like the other constraints.
- **Local and derived.** A puppet is never replicated and never part of a
  checkpoint digest. It is created with the first child that needs it and
  removed with the last. When the device gains the parent's scope, the real
  row replaces the puppet in place.

With partial views, "orders with the customer's name where I can read it" is
`orders LEFT JOIN partial_customers`: an order whose customer is a puppet
returns NULL in the customer columns.

## 6. Bootstrapping and the horizon

**Agreed**, from [the bootstrapping idea](../ideas/bootstrapping.md). The
slices ship in the order A, C, B.

### What exists

`rocky-secure` already has signed per-scope
[checkpoints](../runtime/encrypted-checkpoints.md), a behind-horizon signal and
`NativeSession::rebootstrap`. The SDK's sync never calls it and sets no
retention policy, so nothing prunes and a peer behind a horizon fails.

Strong artifacts are outside this machinery altogether. Commands,
preparations and results are
[never pruned](../runtime/encrypted-checkpoints.md#retries), the checkpoint
statement chains over every retained one, and the rebootstrap export carries
no confirmed command rows. Slice C fixes that.

### Slice A: use it (no new formats)

In `rocky-sdk`:

- Default retention policies.
- `reconcile` rebootstraps a scope that is behind.
- `join` starts from a checkpoint and then catches up.
- A policy epoch change forces a fresh checkpoint, as a key epoch change
  already does.
- `readiness()` reports a bootstrapping state.

This resolves two problems of the idea: a device enrolled after a revocation
receives earlier rows through a checkpoint verified against the current
policy, and a long-offline device adopts a copy instead of replaying history.

### Slice B: dropping tombstones and hidden rows (new format)

- The horizon statement names a cut for a scope and for the
  [existence logs](#existence-log) its rows reference. Every holder syncs those
  logs to the cut and then finalizes the same orphans, so the checkpoints of
  the scope agree, also on devices that cannot read the parents.
- The highest `id` sequence per author and partition survives the horizon
  ([section 3](#the-id)).
- A device whose frontier is behind the horizon must rebootstrap. Otherwise it
  still holds a row whose tombstone others dropped, and resurrects it.
- After the horizon, an edit or delete of a row that does not exist is
  dropped, and an insert that conflicts with an existing row is dropped.
- An edit whose row creation was pruned stays pending forever today. It must
  become a definite drop.
- Dropping a local operation must not end its author's log. It does on the
  trusted path; the encrypted path is unchecked.
- Tombstones alone could be dropped at any cut every device acknowledged,
  because deleted stays deleted. Hidden rows can return, so every device must
  finalize them at **the same cut**. One authority therefore declares the
  horizon with a signed statement, and the others follow.

Slice B changes the checkpoint statement and needs a decision record.

### Slice C: strong history behind the checkpoint (new format)

**Agreed; the first phase after slice A.** It needs slice A and is
independent of slice B. Found by the
[end-to-end qualification](../runtime/end-to-end-evidence.md#device-limits).

Why it is needed:

- A database stops accepting strong commands for good once one device's
  retained commands, preparations and results reach 4,096 records or 32 MiB
  (`command_budget` in `crates/rocky-secure/src/command.rs`). Nothing deletes
  from those tables. The same check runs on every device that imports a
  result, not only on the executor. At the measured 17 KB per result, an
  executor stops after about 940 commands and a reader after about 1,900. An
  application that routes document numbers, stock and postings through strong
  commands ([section 5](#5-constraints-in-mergeable-tables)) passes that
  quickly. Dropping confirmed preparations alone would double the executor's
  life; raising the quotas would only delay the wall and enlarge recovery
  packages. Neither is a substitute.
- The trusted path solved this in
  [decision 0010](../decisions/0010-checkpoints-and-pruning.md#strong-commands):
  its checkpoint keeps the sequence, the head, the latest applied result and
  the unresolved preparations, and pruning deletes applied results once every
  delivery target has applied them. The encrypted path diverged and keeps
  everything, so a peer behind the strong history can only catch up by pulling
  every result from sequence 1. Pruning is therefore a format change.

Design, a port of decision 0010 that reuses `RetentionPolicy`:

1. **The checkpoint carries a strong base.** The statement's `StrongSummary`
   already commits to the confirmed sequence, head and row commitment per
   scope. Its result, command and preparation digests change to cover only the
   records above the base, so two replicas with different pruned prefixes
   still agree. The rebootstrap export adds the confirmed command rows under
   the scope key and the latest result; the import installs them as the
   scope's projection and base.
2. **One horizon per domain.** The lowest sequence that every `required` peer
   not listed in `behind` has applied, less `window`. The executor already
   records per-peer application in `secure_result_ack`; nothing new travels on
   the wire. A strong sequence is totally ordered and confirmed by the
   coordinator, so "the same cut" needs no signed horizon statement for strong
   domains.
3. **What is pruned**, in one step of the existing maintenance: results below
   the horizon whose jobs are all observed; preparations whose result is
   stored, since the result holds the same bytes, and stale preparations of a
   settled request; the invoker's own commands once their ticket is settled.
   The latest result and any result with an unobserved job always stay. A
   retried handle whose command was pruned returns `Error::Pruned`, as a
   mutation handle does today.
4. **Readers start at the base.** The once-per-open audit and the full
   reconcile start at the base instead of sequence 0; the apply path already
   checks the predecessor against the stored head. `missing_results` answers
   below the horizon with the behind signal and the SDK rebootstraps through
   slice A. Recovery packages carry the base and the retained suffix.

What it also fixes:

- **Reopen latency.** Measured on 2026-10-07 in a release build, in process,
  two devices, the invoking device reopened after every 100 confirmed
  commands: its first command after a reopen took 177 ms at 100 retained
  results and 503 ms at 400, about 1.1 ms per result. Open, the first sync and
  every later command stayed flat, at about 15 ms, 12 ms and 40 ms. The growth
  is the once-per-open pass over retained results, and the client evidence
  file did not register; it needs no item. With the base, the pass is bounded
  by `window`.
- **The writer enrolled after a revocation** that reads `Incomplete` today,
  because it cannot fetch results sealed before its enrollment. It
  rebootstraps from a checkpoint verified under the current policy, as slice A
  does for mergeable rows.

It does not shrink a result; that stays with
[large command tables](../optimizations_on_demand/large-command-tables.md).
The horizon is per domain, so it does not pre-empt open question 6 on
partitioned strong tables. Slice C changes the checkpoint statement and the
rebootstrap export and needs a decision record.

### Decided

- **Who may sign a bootstrap checkpoint.** A signer needs a write grant on
  the scope. Today a whole-scope read grant suffices
  ([decision 0011](../decisions/0011-encrypted-checkpoints.md#unit-and-identity)),
  which would let a joiner trust a read-only device for the whole state. No
  new right is introduced yet; an explicit checkpoint right can follow once
  delegation from section 7 exists.
- **Who declares the horizon.** For a mergeable scope the domain's issuer
  signs the horizon statement. A device that has not acknowledged is declared
  behind after the shorter of its credential's remaining validity and 30
  days; it then rebootstraps. For a strong domain slice C decides without a
  statement: the coordinator's confirmed sequence and the executor's
  acknowledgment table.

## 7. Credential primitives

**Agreed.** Permission models belong to the application. Rocky enforces grants
and offers primitives; it does not define roles, groups, membership or sharing
flows.

Needed in `rocky-credentials`, exposed through the native ABI and Kotlin:

| Primitive | State today |
| --- | --- |
| Issue any set of scope, rights and columns, including `Execute` and `Custody` to a device that is not an issuer | Kotlin exposes only the reader, writer and issuer profiles; only issuers execute |
| Delegation that narrows a capability | In the chain format per the [guide](../runtime/credentials.md); not exposed |
| Selectors over partitions, such as "table items, customer X, any warehouse" | New format |
| Key envelopes: wrap a partition key to a recipient key; key epochs per partition | Keys travel only at enrollment and renewal |
| Register an eligible executor with the coordinator | Tied to the issuer profile |
| Author pseudonyms: a separate signing key and author number per device and table, bound in the credential; a range of author numbers per issuer, narrowed by delegation | New format |

Dropped from earlier proposals: named roles in the signed schema, a built-in
host profile, built-in groups and membership, and delegated inviting as an SDK
flow. An application expresses these with the primitives. Roles are plain
application values that produce grant sets, so changing one needs no schema
generation.

Rights are granted to the pseudonym, so the `id` of a row and the records of
an [existence log](#existence-log) do not name a device. Only the issuer can
connect a pseudonym to its device, which keeps accountability.
[Rotation](../ideas/pseudonym_rotation.md) is an idea, not in scope.

Unchanged and not negotiable in the runtime: every frame is verified against
its author's grants, permission columns are protected, and policy epochs
revoke.

The reader, writer and issuer profiles and invitations remain as an optional
starter layer built only on these primitives. `Credentials.prepare` with
`RockyDatabase.install` already lets an application bring its own issuer.

With this, any application instance can carry the schema and handlers and be
granted execution and custody, becoming a trusted node or full replica,
without holding the publisher key.

## 8. Strong commands

### Keyed handler reads

**Agreed.** A handler receives every confirmed row of its tables today, which
keeps command-controlled tables under one 128 KiB envelope. The fix is
deferred to [large command tables](../optimizations_on_demand/large-command-tables.md).
What features_05 does now: the host-language handler interface offers keyed
reads and bounded scans instead of whole tables, served from the complete rows
the runtime already holds. Handlers then do not change when tables grow.
[Independent command sequences](../optimizations_on_demand/independent-command-sequences.md)
are deferred as well.

### Retained history

**Agreed.** Every device stops accepting strong commands once its retained
commands, preparations and results fill a fixed budget, after about 940
commands on an executor. The fix is
[slice C of section 6](#slice-c-strong-history-behind-the-checkpoint-new-format):
results and preparations are pruned behind the checkpoint, which also bounds
the audit a reopened device runs before its first command. Any brief that
puts document numbers, stock or postings behind strong commands depends on it.

### Confirmed rows in Room

**Agreed.** Command-controlled tables are refused by secure SQL installation
today, so Room cannot read confirmed rows. They become read-only tables in
Rocky's file: Room DAOs read stock and document numbers through the same
layer as every other table, and the secure SQL layer refuses any write to
them. Writes stay commands.

### Boundaries

**Agreed.**

- A command's tables are unpartitioned, and the executor holds them
  completely. Partitioned command tables are not in features_05; the horizon
  of slice C is per domain, so the choice stays open for
  [large command tables](../optimizations_on_demand/large-command-tables.md).
- The shopping example stays mergeable-only and needs no coordinator. The
  market app keeps exercising strong commands.
- Optimistic updates are not in features_05; see
  [the idea](../ideas/optimistic_updates.md).

## 9. Platform and operations

**Agreed.** Essential for a real shopping-list application, in this order:

1. **Android.** Only JVM desktop is configured. Needs native packaging,
   lifecycle handling and a Keystore `SecretStore`; keys are plaintext files
   today.
2. **An always-on peer.** Two phones are rarely online together, and pairing
   and renewal need a reachable issuer. Package a custody-only replica, and
   replace the 500 ms sync polling with change- and connectivity-driven sync.
3. **Issuer recovery.** If the creator's device is lost, nobody can invite or
   renew. With section 7 an application can grant issuing rights to a second
   device; the starter layer should do so by default.

## 10. Not in scope

**Agreed.**

- **An own query language.** Not built. SQL stays the surface.
- **Intent capture.** The loss of intent in `SET n = n + 1` is a current
  limitation. Counter columns are postponed; see
  [capturing the intent of SQL writes](../ideas/sql_intent_capture.md).
- **Large command tables and independent command sequences.** Deferred to the
  optimization documents linked in section 8.
- **Uniqueness across partitions** in mergeable tables. Use a strong command.
- **`ON DELETE RESTRICT`** and foreign keys to natural keys.
- **Rotating author pseudonyms**; see
  [the idea](../ideas/pseudonym_rotation.md).
- **A conflict table** for hidden rows.
- **Arbitrary access predicates** and predicate-filtered operation logs.
- **Arbitrary paging** of a table by any sort order; see
  [the idea](../ideas/arbitrary_paging.md). Windowing uses partitions.
- **Optimistic updates** for strong commands. A pure SDK convenience with no
  format impact, so it can follow later; see
  [the idea](../ideas/optimistic_updates.md).
- **iOS.** Android alone adds native packaging, lifecycle handling and a
  Keystore secret store; a second platform doubles the qualification surface
  before the first real app exists. See [the idea](../ideas/ios_platform.md).
- **Partitioned command tables**; see [section 8](#boundaries) and
  [the idea](../ideas/partitioned_command_tables.md).

## Decisions of 2026-10-07

The review questions of the first draft, with their answers. Each is also
written into its section.

1. **Bootstrap checkpoint signer and horizon declaration.** A signer needs a
   write grant; the issuer declares a mergeable horizon, and an absent device
   is behind after the shorter of its credential's remaining validity and 30
   days ([section 6](#decided)).
2. **Sections 8 and 9.** Section 9 stays. Optimistic updates move to
   [section 10](#10-not-in-scope).
3. **iOS.** Later; not in features_05.
4. **Coordinator in the shopping example.** None; the example stays
   mergeable-only. The market app exercises strong commands.
5. **Room access to confirmed command rows.** Read-only, through the same
   DAO layer ([section 8](#confirmed-rows-in-room)).
6. **Partitions and strong commands.** Command tables are unpartitioned and
   held completely by the executor ([section 8](#boundaries)).
7. **Per-scope overhead budget.** Not fixed in advance; the first ERP-shaped
   test holds 200 partitions on one device and sets it
   ([section 4](#known-limits)).
8. **Counter of a table partitioned by its own `id`.** Confirmed: one
   sequence per author and table ([section 3](#the-id)).
9. **Foreign keys across scopes.** A list and its items share one scope per
   list; cross-scope keys are for the ERP cases
   ([section 5](#existence-log)).
10. **Slice C.** Confirmed, directly after slice A
    ([section 6](#slice-c-strong-history-behind-the-checkpoint-new-format)).

## New formats that need decision records

- The `id` and `row_kind` columns, and the `id` as incarnation in frames and
  checkpoints (section 3).
- Partition tuples in scopes, grant selectors and key envelopes (sections 4
  and 7).
- Constraint policies and delete actions in the schema descriptor (section 5).
- The existence log and puppets (section 5).
- Author pseudonyms and author number ranges (section 7).
- The horizon statement and finalization of hidden rows and orphans
  (section 6, slice B).
- The strong base in the checkpoint statement and confirmed command rows in
  the rebootstrap export (section 6, slice C).
