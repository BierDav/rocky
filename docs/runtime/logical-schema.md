# Logical schemas, typed tables and row identity

`rocky_core::LogicalSchema` is the one canonical descriptor both profiles
install. It is built from [definition data](schema-definitions.md) (Kotlin
builder, Room import or JSON), written through [SQL sessions](sql-sessions.md)
or [secure SQL](secure-sql.md), and its secure constraint entries are in
[mergeable constraints](mergeable-constraints.md).

## Descriptor and fingerprints

The canonical form is the JSON encoding of the validated, sorted value;
`parse` refuses bytes that `canonical` would not produce, and more than 4 MiB.

| Identity | Commits to |
| --- | --- |
| `source` | The origin's own identity (Room's `identityHash` and version), uninterpreted |
| `fingerprint` | Everything installed |
| `replicated_fingerprint` | What replicas must agree on: mergeable tables' names, key order, affinity, nullability, key collation, update policy, unique policies and foreign-key delete actions |

A non-unique index, a default, a derived rule or a local table leaves the
replicated fingerprint unchanged. A table or field's name is its identity
forever ([decision 0067](../decisions/0067-names-are-identities.md)): ids are
`definition_item_id(publisher, app, kind, name)`, with `table.field` for
fields. A renamed table or field is another item, and a name added again
means what it meant before, with the same type. The descriptor is stored in
the file at install and another one is refused at open; shapes change
through [generations](generations.md).

Format 5 installs row identity, partition groups, optional `partition_by` and static views
([partitioned scopes](partitioned-scopes.md)). Any other format is
refused by a read-only preflight before database initialization or SDK
enrollment writes.

SQL table and column lookups compare ASCII case insensitively, including
foreign-key targets. Missing target columns report the table and column name.
Ids use names as declared.

## Row identity

Every table, local and command tables included, has `id INTEGER NOT NULL` and
`row_kind INTEGER NOT NULL DEFAULT 0`, supplied by the definition and never
declared. A primary key may name `id`. An unpartitioned table whose key is not
exactly `id` gets a generated unique `<table>_identity` index on `id`; a
partitioned one indexes its partition columns followed by `id`. Both columns
enter the descriptor and its fingerprints. Applications read and write
`row_kind = 0`; kinds 1 (puppet) and 2 (virtual) are
[support rows](mergeable-constraints.md#references-across-reader-scopes).

| Part of an `id` | Bits | Rule |
| --- | --- | --- |
| High bit | 1 | Zero |
| Author number | 31 | Of the node credential; 0 is reserved |
| Sequence | 32 | 1 to 2^32-1 per author and table across its partitions; exhaustion is an error |

There is no timestamp. `RowIdentity` crosses JSON as a canonical decimal
string; SQL binds the positive 63-bit integer. Numeric JSON, leading zeros,
signs and zero are refused.

- `next_id(table)` on either SQL profile reserves an `id` inside an active
  transaction, so a parent can be reserved before its children. Rollback and
  savepoint rollback release reservations; only inserted identities advance
  the author's creation log, and a delete never resets it.
- An application cannot change `id` or `row_kind`.
- A secure node uses its numbered node credential
  ([secure SQL](secure-sql.md#row-identities)); a trusted node binds the
  table's author to its admitted `NodeId` with `TypedApp::with_row_author`,
  the same binding on every replica.
- Receivers check the `id`'s author against the frame signer (secure) and
  admit only sequences above the author's own-birth mark in that partition;
  carries never raise it. Local
  allocation uses the table-wide mark. An exact prior grant and legitimate
  carries follow their own [admission rules](admission-evidence.md). The mark
  commits with the frame, projection and outbox; refused operations never move
  it, duplicates settle first, and pruning, rebuild and checkpoints keep it.
  Phase 17 specifies exact local [admission evidence](admission-evidence.md)
  beside the mark, so a pruned frame this replica admitted can return without
  lowering it (implementation in progress).
- An insert creates an incarnation; updates and deletes name its `id`. Grants
  stay bound to the canonical natural key, never to `id`.
- The typed table shows, per natural key, the live incarnation with the
  greatest creation stamp. Hidden incarnations and tombstones are retained
  until [compaction at a seal](lazy-compaction.md).

Moving an independently partitioned row keeps its `id`, original birth proof
and field stamps. Use the dedicated [row-move surface](row-moves.md), rather
than expressing a move as an ordinary delete and insert.

## Install and inspect (trusted)

`TypedApp::new(schema, domains)` refuses a schema that
`LogicalSchema::supported_on(Profile::Trusted)` rejects; the per-profile
differences are in [schema definitions](schema-definitions.md#check-a-profile-before-installing).

| Refused in a mergeable table | Reason |
| --- | --- |
| Auto-generated key | Concurrent devices would assign the same key |
| Unique index or constraint, `CHECK`, foreign key | Two valid offline writes could not both apply (the secure profile resolves [declared policies](mergeable-constraints.md)) |
| Foreign key on any non-mergeable table, including raw `REFERENCES` SQL | A replicated delete could fail on another table's rows |
| Generated column, NUMERIC affinity, nullable or absent key | No single stored form |
| Local-only column | Checkpoints digest whole rows |
| Collation other than `BINARY`, `NOCASE`, `RTRIM` | Refused at generation |
| A view beyond static SELECT statements | Only static application views are expanded |

Open it with `Replica::open_with_config` or as a `Runtime` domain. Each table
is created from its descriptor's statement. At every open the trusted runtime
compares the descriptor with the engine's catalog (columns, declared affinity,
nullability, defaults, key order, key collation, indexes, foreign keys) and
refuses a scope whose application is not the descriptor's publisher binding.
`Replica::logical_schema()` returns the bound descriptor.

Protocol state is the private `rocky_incarnations` table, which no read can
name: every incarnation with its registers, stamps and tombstone. The typed
table is its projection, written in the same transaction, replayed into the
checkpoint copy and exported with it. A derived column is computed by the
projection write.

## Write rows

SQL sessions derive operations from changed rows. A trusted caller can also
build one operation and submit it with `Replica::mutate`:

| Call | SQL meaning | Operation |
| --- | --- | --- |
| `insert(table, row, &[])` | `INSERT` | New incarnation; refused locally if the key is visible |
| `replace(rows, table, row)` | `INSERT OR REPLACE` | New incarnation replacing every one held at the key |
| `upsert(rows, table, row)` | `@Upsert` | Insert, or one update of the visible incarnation |
| `update_id(rows, table, id, set)` | `UPDATE` of non-key columns | Registers targeting the numeric incarnation |
| `delete_id`, `delete_key(rows, table, key)` | `DELETE` | Tombstones the incarnation, or the visible one and the others held |

`rows` is `Replica::query().rows`: every live incarnation with its key, values
and whether it is visible; `locate` finds a key in it. This reads the whole
domain, whereas a SQL session resolves incarnations inside its transaction.
Inserts carry explicit `id` and `row_kind`. Changing a key value is a delete
and an insert. `upsert` publishes differing columns under `changed` and every
column under `assigned`.

Values are strict: a column stores NULL or the storage class of its affinity.
An integral REAL is accepted as an INTEGER and an INTEGER up to 2^53 in
magnitude as a REAL. A key is at most 1,024 encoded bytes, one operation
64 KiB with at most 16 observed incarnations.

## What converges

| Situation | Result on every replica |
| --- | --- |
| Two devices insert one key | One whole row: the later creation stamp. The other stays hidden |
| Edit of a row deleted elsewhere | No effect; the row stays deleted |
| Key reused after a delete | A new row; earlier edits and tombstones do not reach it |
| Row replaced while another device edits it | The replacement, without that edit |
| Row deleted while another device replaces it | The replacement survives |
| Two devices edit different columns | Both edits |
| `NOCASE` key in two spellings | One row, in the winner's spelling |

Keys compare as SQLite compares them under the declared affinity and
collation (`NOCASE` folds the 26 ASCII letters, `RTRIM` ignores trailing
spaces); there is no Unicode normalization.

## Scalars

`SqlValue` is NULL, INTEGER, REAL, TEXT or BLOB. `Real` cannot be NaN, which
SQLite stores as NULL, and holds negative zero as zero; infinities are kept.
In the JSON SDK a bound REAL is `{"type":"real","value":"<16 hex digits of the bits>"}`.
`to_integer`, `to_real`, `to_text` and `to_blob` follow SQLite's column
accessors. One difference is known: where 15 significant digits do not
identify a double, and for subnormals, the text can differ from the engine's
in its last digits; both read back as the same double. A row digest counts an
integral REAL as its integer, because the engine's row hook reports the
stored integer.

## Qualification

`cargo test -p rocky-sdk --test typed --test sqlite_contract`, against the
pinned SQLite 3.53.2: generation from the checked-in Room export, install,
indexed read, restart and refusal of another descriptor; three devices and a
fourth receiving history backwards agree after concurrent insertion, deletion,
key reuse, replacement and upsert, with checkpoint replay reproducing both
tables; compound, BLOB, `NOCASE`, signed 64-bit and REAL keys conflict in the
engine exactly when their encodings are equal; a dropped index, a rewritten
stored descriptor, a foreign publisher scope and payloads outside the codec
are refused.

## Design rationale

- One descriptor with separate installed and replicated fingerprints lets an
  index, default or SQL spelling change without becoming a protocol change.
- Typed tables are projections of private canonical state, not the state
  itself, so ordinary SQLite readers and ORMs see real keys and indexes while
  merge state stays unreachable. Values are therefore stored twice; a view was
  rejected ([single-copy typed rows](../optimizations_on_demand/single-copy-typed-rows.md)).
- Concurrent inserts at one key keep one whole row and never merge field by
  field: a row mixing two creators' columns was never written by anyone.
- Incarnations are named by a visible numeric `id`, not by the creating
  operation or the key alone: a key-only tombstone made unlinking and
  relinking an N:M pair impossible, and children need a stable parent name
  before the parent's insert commits.
- The `id` embeds the signer's author number and carries no timestamp, so a
  receiver can authenticate who created a row without trusting clocks, and
  allocation needs no persisted allocator (highest own sequence plus one).
- `AUTOINCREMENT` is refused: it takes one more than the largest rowid ever
  held, so a received row moves a local allocation into another device's range.
- Grants stay on natural keys so that a grant never becomes a grant over
  incarnation numbers.
- A file without row identities is never upgraded: its historical inserts
  bind no author number, and inventing one would forge authorship.
