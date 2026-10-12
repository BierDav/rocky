# Unique indexes, foreign keys and hidden rows

Mergeable tables of the secure profile may declare unique indexes with a
conflict policy and foreign keys to a parent's identity. The real SQLite
constraints hold in the typed tables; private merged state keeps every row. A
row kept out of the typed table is *hidden*, as a pure function of the merged
rows, so every replica resolves the same way in any arrival order. The trusted
profile refuses both entries. Resolution lives in
`rocky-secure/src/replica/constraints.rs`; validation in `rocky_core::LogicalTable::installable`.

## Declare them

```kotlin
val Shop = schema("shop") {
    val lists = table("lists") { text("title"); primaryKey("id") }
    table("items") {
        text("code"); text("label", nullable = true); primaryKey("id")
        unique("code", onConflict = Conflict.LatestWins)
        unique("label", onConflict = Conflict.SetNull)
        reference("list", lists, onDelete = Delete.Cascade)
        reference("moved", lists, onDelete = Delete.SetNull)
    }
}
```

Definition data: index `unique` with `on_conflict` (`latest_wins` or
`set_null`); table `foreign_keys` with `columns`, parent `table` and
`on_delete` (`cascade` or `set_null`). `reference` declares an integer column
if absent, nullable under `Delete.SetNull`, and may name the parent by SQL
name. A list of columns names a partitioned parent's identity, its partition
columns first: `reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)`
([partitioned references](#partitioned-references)). `unique` uses Room's default index name or an explicit `name`. After a
Room import, add the policy in the overlay:
`table("items") { onConflict("index_items_code", Conflict.LatestWins) }`. The
importer reads `@ForeignKey` delete actions and parent columns, which must be
the parent's identity; deferred and ordinary Room references both become
deferred native constraints.

Generated statements, compared exactly at installation:

- Unique index: `CREATE UNIQUE INDEX IF NOT EXISTS "<index>" ON "<table>" (<columns>) WHERE "row_kind" = 0`,
  covering real rows only.
- Foreign key: `FOREIGN KEY(<columns>) REFERENCES "<parent>"(<identity>) DEFERRABLE INITIALLY DEFERRED`.
  SQLite checks it at commit and performs no action; Rocky derives the delete
  action. `on_update` is `NO ACTION` because `id` never changes.

| Rule | Refused otherwise |
| --- | --- |
| Index columns exclude `id` and `row_kind` | Yes |
| `set_null` index columns are nullable and outside the primary key | Yes |
| Reference columns are not `id`/`row_kind` and name the parent's identity: the child's partition columns for the parent's, then one INTEGER column for its `id` | Yes |
| Non-partition `SET NULL` reference columns are nullable and outside the primary key | Yes |
| Parent and child are mergeable, in one unpartitioned reader scope, or one explicit partition group with all partition columns mapped by position, or the parent is unpartitioned with an [existence log](#references-across-reader-scopes) | Yes |
| `RESTRICT`, `NO ACTION`, `SET DEFAULT`, natural-key references, other table classes | Always |
| Partitioned policy index includes every partition column and uses `latest_wins` | Yes |
| Partitioned `set_null` index, references between different partition groups, or partitioned child to an unlogged parent outside its group | Always |

Policies and delete actions enter the replicated shape and manifest by
logical names; unconstrained tables keep their manifest items. The SDK puts
reference-connected tables into one deterministic reader scope (the scope hash
of the lexically first SQL table name); a reference to an existence-log table
does not connect them. Grants and keys stay per table. Direct
`EncryptedReplica` callers supply matching scopes at installation. Projection
plans over constrained tables are refused; such a schema changes by export and
reinstall.

## The hidden state

Candidates are the key winners of [row identity](logical-schema.md#row-identity).
Then, for the merged state alone:

1. A candidate is *anchored* unless following its non-null `CASCADE`
   references from candidate to candidate reaches a reference whose parent is
   not a candidate.
2. A candidate *loses* a unique index when it and another anchored candidate
   have equal, non-null indexed values under the columns' collations and the
   other has the later stamp. A row's stamp for an index is the latest register
   stamp of its indexed columns; stamps order by clock, author and sequence; on equal stamps the
   larger `(partition scope, id)` wins; within one partition this is the larger `id`.
3. A candidate is *visible* unless a row reachable through its `CASCADE`
   references, itself included, is not anchored or loses a `latest_wins` index.
4. A visible candidate reads NULL in the referencing columns of a `SET NULL`
   key whose parent is not visible, and in the indexed columns of a
   `set_null` index it loses.

The typed table holds exactly the visible candidates with those values. Hidden
state is never written as an operation or digested by a checkpoint. A change
re-evaluates the rows that depend on it in the same transaction; rebuild,
reopen and checkpoint bootstrap evaluate all of them, so cycles converge in any
arrival order. Each constrained table has a private
`secure_candidates_<table>` table beside the typed one, derived and never
replicated. Candidates, seeds and resolution caches identify rows by
`(table, partition scope, id)`; candidate identity indexes include the partition
columns. A unique index includes all partition columns, so it compares only rows
of that partition, including SQL-equal spellings under its collations. Rebuilding
or evicting one partition replaces only its candidates and typed rows and leaves
other partitions' visible and hidden state unchanged. Resolution reads local
merged rows and requires no other partition's key.

Hidden rows return when the conflicting value changes or a parent
becomes live; a re-created parent has a new `id` and revives no old child.
Hidden rows and orphans are retained until the [horizon](mergeable-horizon.md)
finalizes them.

## Local writes

- A duplicate fails at its statement with SQLite's unique-constraint code
  (2067 through the driver).
- References are deferred: a transaction may insert a child before its parent;
  a dangling reference fails at commit, with SQLite's foreign-key code, before
  anything is sealed.
- A parent delete derives the children's state at commit and publishes only
  the parent deletion.
- A value shown as NULL by a policy is published only when the application
  writes a non-null value, so a whole-entity update keeps the merged value.
- A local write's clock passes the stamps of the incarnations at its key and
  of the rows sharing its unique values, so it wins on its author's device.
- An update publishing any register of a foreign key publishes all of them;
  receivers refuse a partial key.

Remote clashes and missing parents resolve without constraint errors. These
rules give convergence, not invariants: uniqueness across partitions and
anything that must never be violated stay [strong commands](typed-commands.md).

## References across reader scopes

A table declared with an existence log (`existence_log`, Kotlin
`existenceLog = true`, also in an import overlay) may be referenced from
another reader scope. Each row creation and deletion is signed by the table
pseudonym into a log beside the sealed one, and that log alone decides whether
such a row is alive, on holders too. A device holding only the child keeps a
local read-only puppet (`row_kind = 1`) and virtual rows (`row_kind = 2`) so
the real foreign key holds; reads never return them, and a `LEFT JOIN` shows
NULL for a puppet's columns. Existence-log tables, and parents a puppet's
`NOT NULL` references name, must be unpartitioned and keyed by `id`.

**Exposure.** Every authenticated device of the domain can read the log
without the scope key. It reveals the row count, creation and deletion times,
each `id` and the acting pseudonym; never column values. Do not log a table
whose count, timing or authorship is sensitive; keep such parents and children
in one scope or put outside parties in their own domain. Setup, sync and limits
are in [existence logs](existence-logs.md).

## Partitioned references

An in-group reference names the parent's complete identity: partition columns
followed by `id`, once each. Child partition columns map to the parent's by group
slot. `items.list → lists.id` and `(items.list, items.category) → (categories.list,
categories.id)` therefore resolve only within the list partition. `CASCADE` hides
children when that parent disappears; `SET NULL` changes only non-partition
reference columns. A missing or unheld parent partition keeps the child pending,
and a horizon cannot be declared without the corresponding parent partition.
Horizon cuts name concrete parent scopes, never their table templates.

Rebuild and eviction re-resolve constrained tables of the same group partition.
Other partitions remain unchanged. Horizon finalization drops hidden losers and
cascade orphans in that scope; checkpoint import rebuilds candidates and support
in its group partition. An unpartitioned existence-base import rebuilds only its
connected constraint graph. [Partition horizon evidence](mergeable-horizon.md#evidence)
covers eviction, offline rebootstrap and interruption recovery.
A partitioned child may also reference an
unpartitioned logged parent; its liveness and puppets use the existing keyless
log contract. Partitioned existence logs remain deferred. These native shapes
are tested in `rocky-secure`'s `partition_foreign_keys` module. The Kotlin
builder and the Room import admit exactly these shapes and refuse the others
before installation, each with its own message.

## Persisted constraints and open verification

[Decision 0041](../decisions/0041-partition-constraint-state.md) records the format
and catalog boundary. The current private support bookkeeping shape is `secure_support(tab, scope, id)`
with that composite primary key. Puppet and virtual-row lookups use the full
partition-and-id identity; partitioned support tables remain refused. There is no migration or alternate encoding. At open, secure constrained
installations compare the declared typed indexes, private candidate tables and
indexes, and support bookkeeping table to SQLite's stored SQL. Missing or changed
identity/policy indexes or an older private table shape fail open before use.
Reinstall an incompatible installation.

When one frame writes different unique-index columns of two conflicting rows,
the index stamps can be equal. The larger row id wins that tie consistently
on receipt, projection rebuild and reopen.

## Qualification

- `cargo test -p rocky-secure --lib partitioned_`: offline duplicate titles in
  two partitions, equal ids, A-only readers, equal-stamp ties, local SQL refusal
  and rollback, actual custody-backed eviction, partition rebuild and reopen,
  and missing-index refusal.
- `cargo test -p rocky-secure --lib tests::constraints`: reversed-delivery
  clashes under both policies, loser reappearance, parent deletion concurrent
  with child insertion, repointing, recreated parents, child-before-parent
  delivery, local refusal, reopen, checkpoint bootstrap and eviction/rebuild.
- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:check`: the Room
  `ConstraintDatabase` fixture with both policies and delete actions imports to
  the hand-built descriptor, installs natively, reopens, and is refused on the
  trusted profile.
- `cd examples/shoppinglist-app && ./kotlin run -m qualification -- required-schema`: the scope's shopping schema, hand-built and
  imported from Room with its overlay, has identical bytes and installs
  natively from either; `ShoppingIntegrationTest` (`cd examples/shoppinglist-app && ./kotlin test -m shared`)
  covers its DAOs, per-list titles, `SET NULL` categories and cascading lists.
- `cargo test -p rocky-sdk --test driver room_reads_each`: Room's
  `foreign_key_list` reads each reference's own delete action when references
  share a partition column.
- `RoomConstraintsTest` (`sdks/kotlin/gradlew -p sdks/kotlin :qualification:room:jvmTest`): DAO inserts,
  local unique and dangling refusal, parent deletion, `SET NULL`, `CASCADE`
  and reopen through Rocky's driver.

## Design rationale

- Real SQLite constraints in the typed table, rather than emulation, keep ORM
  and raw-SQLite readers correct; hiding rows is the only way two valid offline
  writes can both be kept and the constraint still hold.
- Hidden state is derived, never replicated, so no operation can encode a
  resolution that another replica would compute differently.
- SQLite performs no delete action itself (deferred `NO ACTION`): an engine
  cascade would delete merged rows instead of hiding them reversibly.
- Losing is decided among *anchored* candidates, not visible ones, which keeps
  the rules acyclic; a row hidden through its parent can still make an equal
  row lose.

Partitioned `SetNull` indexes are refused in phase 4: nulling the entire index
would change its partition identity. Foreign keys admit in-group identities
and unpartitioned logged parents, natively and in the Kotlin builder and Room import.
