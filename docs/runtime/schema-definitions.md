# Schema definitions: JSON, Kotlin builder and Room import

A [logical schema](logical-schema.md) is built from *definition data*: JSON any
language can write (`rocky_core::SchemaDefinition`). The Kotlin builder and the
Room 2.8.4 importer both emit it; for equal declarations the canonical
descriptor and both fingerprints are identical.

## Write a definition

```json
{
  "format": 6,
  "publisher": "6b6b…6b",
  "application": "notebook",
  "update_policy": "changed",
  "tables": [
    {
      "name": "notes",
      "class": "mergeable",
      "columns": [
        {"name": "key", "type": "text", "not_null": true},
        {"name": "folder", "type": "text", "not_null": true, "default": "inbox", "collation": "nocase"},
        {"name": "body", "type": "text"}
      ],
      "primary_key": ["key"],
      "indexes": [{"name": "notes_by_folder", "columns": [{"name": "folder", "descending": false}]}]
    }
  ]
}
```

| Entry | Meaning |
| --- | --- |
| `format` | 6, including partition groups, optional `partition_by` and `views` ([partitioned scopes](partitioned-scopes.md)) and the SDK schema's `granted_like` and `migrations` ([generations](generations.md)); others are refused. The built descriptor is format 5 |
| `publisher`, `application` | Bind table and field ids; 64 hexadecimal digits |
| `update_policy` | `changed` (default) or `assigned`; see [SQL sessions](sql-sessions.md#what-a-commit-publishes) |
| table and column `name` | The identity forever: a renamed item is another one, and a name added again keeps its meaning and type ([decision 0067](../decisions/0067-names-are-identities.md)) |
| table `class` | `mergeable`, `local_only`, [`command_controlled`](confirmed-tables.md) or `confirmed_read_only` (trusted only) |
| column `type` | `integer`, `real`, `text`, `blob`; `numeric` only in local tables |
| column `not_null`, `default`, `collation` | A JSON scalar default; `binary`, `nocase` or `rtrim` |
| column `derived` | An expression over the row's replicated columns (trusted only) |
| column `local_only` | Declared, and refused inside a mergeable table |
| index `unique`, `on_conflict`; table `foreign_keys` | [Mergeable constraints](mergeable-constraints.md), secure only |
| table `existence_log` | Other reader scopes may reference it; see [existence logs](existence-logs.md) |
| table `partition_group` | Explicit group name; `partition_by` maps columns by position to matching affinity/collation slots ([groups](partitioned-scopes.md#partition-groups)) |

Do not declare `id` or `row_kind`: the definition supplies both and the
identity index ([row identity](logical-schema.md#row-identity)). A primary key
is explicit and non-null. Auto-generated keys have no entry.

`SchemaDefinition::parse(bytes)?.build()?` returns the canonical descriptor:
the same definition always builds the same bytes, with generated `CREATE`
statements, source kind `rocky.definition` and a commitment to the definition
as its identity. Tables are emitted in name order and indexes in name
order; column and key order are part of the declaration. Install it with
`TypedApp::new(schema, domains)` on the trusted runtime or `TypedRows::new`
with `EncryptedReplica::install_tables` on the secure one. The SDK schema
input takes the same `tables`, `update_policy` and `views`, binds the publisher
at enrollment and builds the descriptor. Input is bounded at 1 MiB, 64 tables
and 128 columns per table; register-style `collections` are refused. It also
takes `granted_like` and `migrations`, which change no descriptor; see
[generations](generations.md#tables-granted-like-another).

## Build in Kotlin

```kotlin
val Samples = schema("samples") {
    table("samples") {
        text("sensor"); integer("sequence")
        real("reading", nullable = true); blob("raw", nullable = true)
        primaryKey("sensor", "sequence")
        index("samples_by_reading", listOf(IndexColumn("reading", descending = true)))
    }
}
```

`boolean` declares an INTEGER column, with Boolean defaults normalized to 0/1.
`column(Column.text(...).copy(default = JsonPrimitive(...)))` adds defaults,
collation or derived metadata. `tableClass`,
`existenceLog`, `partitionBy`, `view` and `updatePolicy` set the definition
attributes; `unique`, `onConflict` and `reference` declare
[constraints](mergeable-constraints.md#declare-them).
`definitionJson(publisher)` exports the definition for other consumers.

## Import a Room export

```kotlin
val Shopping = schemaFromRoom("shopping", exportedJson) {
    table("shopping_items") { existenceLog = true }
}
```

`schema("shopping", version = 2) { fromRoom(exportedJson); ... }` is the same
import inside a builder; `schemaFromRoom` uses the exported database version.
Room supplies types, defaults, collations, key order, indexes and identity
references; Rocky attributes are added in the overlay, with no policy JSON.
Imported tables start as mergeable. `table(name)` reopens a declaration and
`configureColumn` changes attributes without repeating the stored type;
re-declaring a column or index fails.

| Room export | Import |
| --- | --- |
| Ordinary tables with TEXT, INTEGER, REAL, BLOB columns and explicit non-null keys | Accepted; Boolean is INTEGER |
| Defaults | SQL scalar literals only; expression and BLOB defaults are refused |
| Collations | `BINARY`, `NOCASE`, `RTRIM` |
| `id` and `row_kind` entity fields | Required as non-null integers; removed from the business columns |
| `<table>_identity` index on `id` (natural keys) | Required and dropped, since the definition supplies it; partition columns precede `id` under a partition overlay ([Room entities](room-integration.md#entities)) |
| Unique index | Needs an `onConflict` policy in the overlay |
| `@ForeignKey` | Must reference the parent's identity, its partition columns then `id`, with `CASCADE` or `SET NULL` and `NO ACTION` on update; the schema checks it against the overlay's `partitionBy` |
| `@DatabaseView` | One static SELECT, nested views included; other shapes are refused |
| `CHECK`, generated columns, virtual/FTS tables, partial or expression indexes, `autoGenerate = true` | Refused, naming the entity |
| Rocky, Room and SQLite bookkeeping tables, `android_metadata` | Excluded |

The importer validates exported metadata against the table and index SQL.

## Check a profile before installing

`schema.diagnose(Profile::Trusted | Profile::Secure)` lists every refusal and
`supported_on` returns the first. Both constructors call it, so a refused
schema never creates a file or reaches remote apply. The SDK admits schemas
for the secure profile only.

| Descriptor feature | Trusted | Secure |
| --- | --- | --- |
| Mergeable, local-only and command-controlled tables | Yes | Yes |
| Confirmed read-only tables | Installed | Refused |
| Derived columns | Computed at commit | Refused |
| Policy unique indexes, identity foreign keys, existence logs | Refused | [Resolved by hidden rows](mergeable-constraints.md) |
| `CHECK`, plain unique constraints and auto keys in mergeable tables | Refused | Refused |
| Declared foreign keys on non-mergeable tables | Refused | Refused |
| Static views | Expanded | Expanded |
| Engine catalog verified against the descriptor at open | Yes | No; the stored descriptor is compared |
| Read scope | The whole domain | Whole rows of each table's reader scope |

Both profiles store strict values and compare keys as described in
[logical schemas](logical-schema.md#write-rows), and both keep incarnations,
so a deleted key may return as a new row. Local tables keep SQLite's unique,
check and default constraints. A secure reader key covers whole rows of one
table: finer read views would not make a shared key confidential per column,
so none exist.

## Qualification

- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:check`: compiles the importer
  with `-Werror`; hand-built and imported shopping, market, scalar,
  partitioned, grouped, `ConstraintDatabase` and three-table shopping fixtures have equal canonical bytes and
  fingerprints; named refusals (`autoGenerate`, a missing identity field, a
  missing or misnamed identity index); each partitioned constraint shape that
  stays refused, with its message; each schema installs natively and
  reopens with its import.
- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:jvmTest`: regenerates the checked-in exports
  with Room 2.8.4 and checks REAL/BLOB/null values and compound keys through
  Room DAOs on `BundledSQLiteDriver` (Room's value shape, not Rocky's driver);
  a hand-built definition opens through JVM UniFFI and reopens with its import.

## Design rationale

- Definition data, not Room, is the schema contract, so a consumer needs no
  Rust project, ORM or policy file; Room is one importer among possible ones.
- Unsupported features are refused by the profile check before any file
  exists, never installed with a weaker guarantee.
- Merge rules are built in (per-field last writer wins under `changed` or
  `assigned`); custom reducers were rejected for definition tables. Invariants
  belong to [typed commands](typed-commands.md).
