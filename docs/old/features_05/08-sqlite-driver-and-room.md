# 8. SQLite driver and Room

Status: complete. Slices 1–5 and 7 passed the completion gate on 2026-10-08;
slice 6, the entity rules of [phase 7](07-row-identity.md), landed after that
phase's gate on the same day (evidence below). Owner: `sdks/native` typed handles, the new
`sdks/kotlin/rocky-sqlite` and the thinned `sdks/kotlin/rocky-room`. Depends
on [phase 2](02-schema-builder-and-room-import.md); takes the entity rules of
[phase 7](07-row-identity.md) as a late slice. Wave 1. The largest novel
engineering item; budget it as such.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 1](agreed-scope.md#1-sqlite-driver-and-room) of the scope,
[Room integration](../runtime/room-integration.md),
[queries and observation](../runtime/queries-and-observation.md) for snapshot
cursors, catalogs and table events, [SQL sessions](../runtime/sql-sessions.md),
[secure SQL](../runtime/secure-sql.md) for leases and limits, the
[pinned Room findings](../old/features_03/research.md#exact-room-version-inspected)
and the old Room recording fixtures, which are the acceptance reference.

Room has no database of its own: its connections open Rocky's file through a
Rocky `androidx.sqlite.SQLiteDriver`. One Room transaction is one commit and
one signed bundle. Capture stays in the Rust engine below the driver; an
unsupported call is an error, never a silent unreplicated write.

## Ordered slices

1. `sdks/native`: typed statement, cursor and table-event handles; the JSON
   `call` per step is too slow for a driver.
2. `rocky-sqlite` without a Room dependency: writer connection as a SQL
   session, reader connections as snapshot cursors, nested transactions as
   savepoints, bundle limits (2,048 rows, 4 MiB, net change) and the 5 s write
   lease surfaced as driver errors.
3. Room housekeeping answered from the session catalog for the pinned
   version: `PRAGMA user_version`, `table_info`, `index_list`,
   `room_master_table`, `CREATE TABLE IF NOT EXISTS`.
4. Invalidation: emulate Room's modification log from Rocky table events so
   sync writes invalidate DAO observers.
5. `rocky-room` becomes thin: schema import from phase 2, housekeeping and
   invalidation. The shopping example moves to plain DAOs.
6. After phase 7's gate: refuse `autoGenerate = true`, require `id` and
   `row_kind` fields on every entity, and let a DAO obtain an `id` before
   insert in one transaction.
7. Remove the projection adapter (`RoomReplica`, `PendingMutation`,
   `ReplicaMetadata`) once the driver qualifies. Two maintained paths are not
   kept.

## Completion gate

- The old Room recording fixtures pass through the driver for the pinned
  version; every housekeeping statement is answered, none `Denied`.
- A Room transaction with parent and children is one bundle on peers; an
  oversized transaction fails and is not split; a row inserted and deleted in
  one transaction publishes nothing.
- A whole-entity `@Update` under the `changed` policy publishes only changed
  columns.
- DAO observers fire on sync writes; reader connections never block the writer.
- The file opens with ordinary SQLite tooling and shows real keys and indexes.
- [Room integration](../runtime/room-integration.md) is rewritten for the
  driver; the projection adapter is gone from code and docs.

## Measured evidence

Slice 1 (2026-10-08): typed handles. `rocky_sdk::Database::connect` serves
writer and reader [driver connections](../runtime/sql-driver.md) on the
database's reactor; `sdks/native` exports them as `NativeConnection`,
`NativeStatement` and `NativeTableEvents` with typed `NativeValue` pages
and `NativeError.Sql` codes. Snapshot cursors gained same-snapshot statements
for read transactions. `cargo test -p rocky-sdk --test driver` passed 5 tests
(one bundle per transaction, savepoints, net change, bundle limits, lease
timeout, reader snapshots, peers); `cargo test -p rocky-storage --lib cursor`
passed 5. The native crate builds and passes Clippy; its check script is
verified out of band.

Slice 2 (2026-10-08): `sdks/kotlin/rocky-sqlite`, an `androidx.sqlite` 2.6.2
driver with no Room dependency. Writers are SQL sessions, readers (marked by
`PRAGMA query_only = 1`) step snapshot cursors, nested transactions are
savepoints, and bundle limits and the lease are `SQLiteException`s with
SQLite codes. `./kotlin test -m rocky-sqlite` passed 5 tests, including getter
parity with `BundledSQLiteDriver`; the Android packaging publishes
`rocky:rocky-sqlite` (not rebuilt here).

Slice 3 (2026-10-08): the `room-2.8.4` connection profile answers Room's
housekeeping from the session catalog; schema pragmas read the real file.
`PRAGMA query_only` switches a connection between writer and reader, which is
how Room marks its readers after configuring them. The recording fixture,
which did not exist in code before, now does: 47 statement shapes of Room
2.8.4 over the bundled driver, pinned and replayed. `./kotlin test -m
room-qualification` passed 3 tests twice in a row; `cargo test -p rocky-sdk
--test driver` passed 6.

Slice 4 (2026-10-08): invalidation. The profile's emulated modification log,
which landed with slice 3 because Room's open path sends it, marks tracked
tables from committed table events; `rocky-room`'s `followRocky` refreshes
Room's tracker after every commit. `./kotlin test -m room-qualification`
passed 4 tests: a DAO `Flow` on a joined device emits after its own write and
after a synced insert and update. A one-off run with `followRocky` disabled
timed out at the first synced write, as expected.

Slice 5 (2026-10-08): the shopping example runs on plain DAOs. Its
`ShoppingSchema` is the builder's declaration of the Room entity, equal to
importing the export; `ShoppingRepository` calls `@Insert` and `@Query`
DAOs, `ShoppingDesktop` builds Room with `setRockyDatabase` and runs
`followRocky`, and the Android shopping app and its host peer (which imports
the same export and writes with SQL) follow. `rocky-room` is the Room profile,
invalidation and, through `rocky-core`, the phase 2 importer; its Android
packaging now compiles against and depends on `rocky-sqlite`.
`./examples/shoppinglist-app/check` passed 20 tests and the desktop build:
offline DAO edits on two devices converge field by field and a delete beats
a concurrent rename; a filtered DAO flow follows a synced insert and a local
update. `./sdks/kotlin/check-schema` passed with the regenerated export. A
stale identity-version assertion (7, since phase 1 writes 8) in
`CredentialPrimitivesTest` was corrected.

Slice 7 (2026-10-08): the projection adapter is removed. `RoomReplica`,
`RoomProjection`, `RoomTable`, `PendingMutation` and `ReplicaMetadata` are gone
from `rocky-room`, which no longer runs Room's compiler or exports the bundled
SQLite driver; the qualification modules take that driver as a test
dependency. [Room integration](../runtime/room-integration.md) is rewritten
for the driver, the historical adapter plan is deleted, and the module map,
READMEs, SDK contract, schema builder and Android guides name the single path.

Completion gate (2026-10-08), `./kotlin test -m room-qualification`, 9 tests,
with `cargo test -p rocky-sdk --test driver` for the engine side:

| Gate | Evidence |
| --- | --- |
| Recording fixtures through the driver, none `Denied` | The pinned 47-shape Room 2.8.4 fixture matches a fresh recording; its trace replays through the driver with no housekeeping failure and engine-equal DAO results; the DAOs run live with only the upsert probe's 1555 |
| Parent and children are one bundle on peers | A `@Transaction` of a list and three items adds one bundle to `secure_bundle_handles`; the peer applies it as one table event naming `items` and `lists` |
| Oversized transactions fail whole | 2,049 items in one `@Insert` fail with code 18; no bundle, no row on either device |
| Insert and delete publish nothing | One Room transaction inserting and deleting an item adds no bundle |
| Whole-entity `@Update` under `changed` | Two devices update the whole entity, changing different columns; both converge to both changes |
| DAO observers fire on sync writes | A DAO `Flow` emits for synced inserts and updates through `followRocky`, and does not without it |
| Readers never block the writer | A DAO write commits within 2 s inside an open Room read transaction, which keeps its snapshot |
| Ordinary SQLite tooling | The bundled driver opens `rocky.sqlite` read-only: `PRIMARY KEY` in the table SQL, `table_info`, `index_list` and `index_info` of `items_by_list`, a query plan using it, and the rows |
| Docs and code have one path | Slice 7 |

On Android (2026-10-08, the phase 5 API 35 arm64 emulator), the since-removed
device gate passed with the shopping app on Room DAOs over the packaged `rocky-room` and
`rocky-sqlite`: joined and showed the host's row in 0.6 s, the host received
the phone's in 0.4 s, background pause, and a cold restart that resumed and
showed the row added while dead in 0.6 s; the todo app and the instrumented
debug and release checks passed unchanged.

Slice 6 (2026-10-08): the entity rules, after phase 7's gate. On main the
phase 7 rules had left `./kotlin test -m rocky-sqlite` at 0 of 5 and
`room-qualification` at 1 of 9 (text `id` entities refused), and
`sdks/native/Cargo.lock` lacked phase 7's `rocky-storage` dependency, so the
Kotlin wrapper could not build the native library; this slice repairs both.

- Row ids before insert: a driver writer answers `SELECT rocky_next_id('t')`
  or `rocky_next_id(?)` inside its write transaction with the table author's
  next id ([row ids](../runtime/sql-driver.md#row-ids)), with no profile, so
  any ORM can use it. It refuses outside a transaction (code 1), on a reader
  (8) and for a bad argument; a rollback releases the id. Later reservations
  of one transaction continue the first instead of verifying the author chain
  again; the commit verifies every inserted id. A Room DAO calls it from a
  `@Transaction` method.
- `autoGenerate = true` is refused at import and, as `AUTOINCREMENT`, in Room's
  `CREATE TABLE`; an entity without the `id` or `row_kind` field is refused at
  both; a natural-key entity must declare the identity index `<table>_identity`
  ([entities](../runtime/room-integration.md#entities)).
- The qualification, shopping and schema-qualification entities carry `id` and
  `row_kind`; the Room 2.8.4 fixture was re-recorded for that shape (49
  statements; the pinned version is unchanged). Historical text-id exports are
  kept as refusal fixtures under `sdks/kotlin/fixtures/schema/legacy`.

`cargo test -p rocky-sdk --test driver` passed 7 tests, including id
reservation and the housekeeping refusals. `./kotlin test -m room-qualification`
passed 10 tests twice in a row: the recorded trace replays with every
housekeeping statement answered; the DAOs run on Rocky's file with the ids
Rocky issues and fail only where the bundled engine does; a DAO reserves its
ids in the inserting transaction (one commit; a rollback releases; an unissued
or skipped id is refused and never reaches the peer); parent and children are
one bundle; 2,049 items fail with code 18; a whole-entity `@Update` under
`changed` publishes only changed columns; DAO flows fire on sync writes;
readers never block the writer; ordinary SQLite shows the identity index.
`./kotlin test -m rocky-sqlite` passed 5, `./kotlin test -m
schema-qualification` 2, and `./sdks/kotlin/check-schema` passed with the new
refusals. In `./examples/shoppinglist-app/check` the Room tests pass (offline
DAO edits converge, filtered flows follow sync, schema equality, issuer
recovery), and the desktop app builds. 14 tests on SDK register schemas,
credential primitives and reservation commands still fail as on main, with
phase 7's `legacy register schemas are unsupported`; they do not use Room.

The debug native build steps about 2.7 ms per single-row DAO insert on the
JVM, so the oversized-transaction test runs on a 60 s lease; release is about
ten times faster. Android was not rerun: the host peer still inserts text ids,
and a non-issuing phone has no network path to obtain a table pseudonym.
