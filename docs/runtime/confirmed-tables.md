# Confirmed tables

A table of class `command_controlled` holds the confirmed rows of
[typed strong commands](typed-commands.md) as an ordinary typed table in
Rocky's file. Room DAOs, driver readers and SQL sessions read it like any
other table. Nothing but a confirmed command result changes it.

## Declare one

Mark the table in the definition; the rest of the declaration is a mergeable
table's: a primary key, Rocky's `id` and `row_kind`, no unique constraint,
foreign key, local or derived column.

```kotlin
val schema = schemaFromRoom("market", export) {
    table("stock") { tableClass = TableClass.CommandControlled }
    commands = CommandSet(listOf(Restock))
}
```

Definition tables use `"class": "command_controlled"`. Name the table in the
`resources` of the commands that write it. A command's tables are
unpartitioned and the executor holds them completely.

The class is signed: the table's manifest entity carries it, so a device that
declares the table mergeable has another manifest and is refused. Mergeable
entity shapes are unchanged. A schema transition cannot add, remove, reshape
or reclassify a command-controlled table; a projection plan keeps its
statement and may only change its indexes.

## Reads

The table shows the scope's confirmed command state: the rows of the latest
result this device applied, in confirmed order. It changes in the transaction
that applies a result, with the result's revision, and table events name it
with cause `confirmed`, so `followRocky` refreshes DAO flows.

| Situation | Table |
| --- | --- |
| Ticket queued, prepared or outcome unknown | Unchanged |
| Confirmed, predecessors not here yet | Unchanged until they arrive |
| Result applied | The result's rows, atomically with the application |
| Application failed or rolled back | Unchanged |
| Results confirmed before installation | Shown when the tables are installed |
| Strong state imported from a checkpoint or recovery | Its rows, in the import's transaction |

There are no optimistic overlays of pending commands. A scope this
device holds no key for reads as `Unavailable`, as other typed tables do.
`command_rows` and `command_snapshot` keep reading the audited state with its
`complete` flag.

## Writes

Secure SQL refuses every `INSERT`, `UPDATE` and `DELETE` on the table at the
statement, whatever the session's grants. The refusal is `SqlError::Denied`,
SQLite code 23 (`SQLITE_AUTH`) through the driver and Room. Inside a
transaction it ends the transaction: its other statements roll back and its
commit publishes nothing. `next_id` and `rocky_next_id` refuse the table.
Session catalogs list it with `writable = false`. A row write that reaches
admission another way, such as a peer's frame, is refused: only confirmed
results change command-controlled rows.

## Qualification

- `cargo test -p rocky-secure --lib confirmed_tables`: projection at install,
  on confirmation, after a checkpoint import and across reopen; a failed application changes nothing;
  pending and unapplied results never show; each write kind is refused alone
  and inside a transaction with a valid mergeable insert, which publishes
  nothing; admission refuses row writes; a projection plan keeps the table.
- `cargo test -p rocky-core command_controlled`: signed class, installation
  rules and refused transitions.
- `cargo test -p rocky-sdk --test confirmed_tables`: driver readers and the
  writer read confirmed rows, writes fail with code 23 and publish no bundle,
  table events carry `confirmed`.
- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:room:jvmTest` (`ConfirmedRowsTest`): a DAO `Flow`
  follows confirmation, every DAO write kind fails with code 23, the
  transaction publishes nothing and the mergeable table beside it stays
  writable.
