# 13. Confirmed rows in Room

Status: complete on 2026-10-08. Slices 1–3 are qualified through the driver and
Room, and the market app reads its stock through a Room DAO (evidence below).
Owner: `rocky-secure` installation and secure SQL, `rocky-sqlite`. Depends on [phase 8](08-sqlite-driver-and-room.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [confirmed rows in Room](agreed-scope.md#confirmed-rows-in-room) and
[boundaries](agreed-scope.md#boundaries) of the scope,
[secure SQL](../runtime/secure-sql.md) and
[typed commands](../runtime/typed-commands.md). Command-controlled tables are
refused by secure SQL installation today.

Command-controlled tables become read-only typed tables in Rocky's file. Room
DAOs read stock and document numbers through the same layer as every other
table; the secure SQL layer refuses any write to them. Writes stay commands.
Command tables are unpartitioned and held completely by the executor.

## Ordered slices

1. Installation accepts command-controlled tables as read-only typed tables;
   the projection follows confirmed results only, never offline overlays.
2. Secure SQL refuses `INSERT`, `UPDATE` and `DELETE` on them at the
   statement, also inside a transaction that otherwise succeeds.
3. The driver exposes them to Room; the market app reads its stock through a
   DAO and observes confirmation through invalidation.

## Completion gate

- A DAO reads confirmed rows; a write through Room fails with a typed error
  and the transaction publishes nothing.
- A confirmed result updates the typed table atomically with its application
  and invalidates DAO observers.
- Pending commands never show in the table; optimistic updates stay
  [out of scope](agreed-scope.md#10-not-in-scope).

## Measured evidence

Implemented 2026-10-08; the contract is [confirmed tables](../runtime/confirmed-tables.md)
and the format amendment is in
[decision 0025](../decisions/0025-typed-commands-tickets-and-handler-hosts.md#command-controlled-tables-2026-10-08).

Slice 1: `command_controlled` tables pass secure installation with the
mergeable table rules. Their entities enter the signed manifest with the class
in the shape (mergeable shapes unchanged), and transitions that add, change or
reclassify them are refused. `TypedRows` holds them; its row-write validation
refuses them and `RowCommandApplication::validate_confirmed` admits confirmed
results. The typed table is replaced from `secure_command_rows` in the
transaction that applies a result, at installation, and on rebuild; eviction
and projection plans leave it in place.

Slice 2: the storage gate already denies writes to non-writable classes, so
`INSERT`, `UPDATE` and `DELETE` fail at the statement with `SqlError::Denied`
and end the transaction; `next_id` refuses the table.

Slice 3: the driver needs no change; Room reads the table through the
`room-2.8.4` profile and `followRocky` refreshes DAO flows from table events
with cause `confirmed`.

| Gate | Evidence |
| --- | --- |
| A DAO reads confirmed rows | `ConfirmedRowsTest`: `MarketDao.stock()` and `observeStock()` show the restocked row after `run`, equal to `confirmedRows("stock")` |
| A write through Room fails with a typed error and publishes nothing | `@Insert`, `@Update`, a deleting `@Query` and a `@Transaction` with a valid hold insert each fail with `SQLiteException` code 23; `secure_bundle_handles` and `holds` are unchanged; the same in `cargo test -p rocky-sdk --test confirmed_tables` and `rocky-secure` `secure_sql_refuses_every_write_to_a_command_controlled_table` |
| A confirmed result updates the table atomically and invalidates observers | `confirmed_results_project_atomically_and_pending_commands_never_show`: an injected failure before commit leaves the table and revision unchanged and fires no event; the retry applies; observers get `stock` with cause `confirmed`; the DAO `Flow` follows both restocks |
| Pending commands never show | A queued ticket, and a confirmed ticket whose predecessors have not arrived, leave the table empty until catch-up applies them |

Runs: `cargo test -p rocky-secure --lib confirmed_tables` 3 passed;
`cargo test -p rocky-core command_controlled` 2 passed;
`cargo test -p rocky-sdk --test confirmed_tables` 1 passed;
`./kotlin test -m room-qualification` 11 passed, with the Room 2.8.4 fixture
tests unchanged. `cargo test -p rocky-core -p rocky-secure` passed in full.

Historical market app (removed 2026-10-08; slice 3): migrated to format 2 and builds
as a Kotlin toolchain module on `rocky-room` with Room 2.8.4's KSP compiler.
`stock` is command-controlled; `src/Stock.kt` declares its entity, `StockDao`
and one Room database per schema version, since Room's version is the installed
schema's. Each device process opens Room on its own `RockyDatabase`, runs
`followRocky` and collects `StockDao.observe()`. Its now-removed gate asserted:

- while `Restock` is queued the DAO reads no row; after it is applied the DAO
  `Flow` has emitted `[]` then `widget=1`, and `StockDao.all()` equals
  `confirmedRows("stock")`;
- after the competing reservations the `Flow` emits `widget=0`; a DAO
  `@Insert` fails with `Error code: 23` and an SQL insert is refused;
- the recovery restock `gadget=10` reaches the same `Flow`.

Migrating the example also showed that author pseudonyms must be installed
again after a schema upgrade (the chain is bound to the manifest) and after a
renewal into a new policy epoch (as [credential primitives](../runtime/credential-primitives.md#author-pseudonyms)
state); the example does both, keeping the same author numbers.

Runs on the final tree (2026-10-08): the market gate passed 42 checks with no
recorded limit; `./kotlin test -m room-qualification` 11 passed;
`./kotlin test -m schema-qualification` 2 passed; `./sdks/kotlin/check-schema`
passed; the slice 1–2 tests above passed again. Two earlier market runs each
failed one timing-dependent recovery check unrelated to Room (an executor
restart outlasting the 60 s admission window), under load from other work on
the machine; the same checks passed in the final run.
