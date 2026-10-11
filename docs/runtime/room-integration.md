# Room on Rocky's file

Room has no database of its own: its connections open Rocky's file through
Rocky's [SQL driver](sql-driver.md). DAOs read and write the replicated typed
tables, and one Room transaction is one Rocky commit and one signed
[bundle](secure-sessions.md#bundles). Room is optional under
[V4](../proposals/architecture-proposal-v4.md); any ORM on `androidx.sqlite`
can use the driver without Room.

Pinned: Room 2.8.4 with androidx.sqlite 2.6.2, qualified on the JVM desktop
(macOS ARM64) and packaged for [Android](android.md); iOS is not configured.

## Start

Declare entities and DAOs as usual, then derive Rocky's schema from Room's
export, or declare the same tables with the [builder](schema-definitions.md#build-in-kotlin):

```kotlin
@Entity(tableName = "shopping_items")
data class ShoppingItem(@PrimaryKey val id: Long, val title: String, val completed: Boolean,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0)

@Dao interface ShoppingDao {
    @Insert suspend fun insert(item: ShoppingItem)
    @SkipQueryVerification @Query("SELECT rocky_next_id('shopping_items')") suspend fun nextId(): Long
    @Transaction suspend fun add(title: String) = ShoppingItem(nextId(), title, false).also { insert(it) }
    // …
}
@Database(entities = [ShoppingItem::class], version = 2, exportSchema = true)
abstract class ShoppingDatabase : RoomDatabase() { abstract fun items(): ShoppingDao }

val schema = schemaFromRoom("shopping", exportedJson)
val rocky = RockyDatabase.create(dataDirectory, schema)
val room = Room.databaseBuilder<ShoppingDatabase>(File(dataDirectory, "shopping").path)
    .buildOnRocky(rocky)

room.room.items().add("Milk")
room.room.items().observe().settledOnRocky().collect { items -> /* local and synced rows */ }

room.close(); rocky.close()
```

`setRockyDatabase` installs `RockySQLiteDriver` with the `room-2.8.4` profile
and runs queries on the IO dispatcher. The name passed to Room names no data;
Room keeps only its `<name>.lck` lock file there. `RockyDatabase` owns the
native engine, enrollment and sync; close Room before it. The
[shopping example](../../examples/shoppinglist-app/README.md) is a complete
desktop and Android consumer.

## Libraries and ownership

| Module | Owns |
| --- | --- |
| `sdks/kotlin/rocky-core` | Schemas, the builder and the [Room export importer](schema-definitions.md#import-a-room-export), `RockyDatabase` and the ORM-neutral suspend/Flow API; no Room dependency |
| `sdks/kotlin/rocky-sqlite` | The `androidx.sqlite` driver; no Room dependency |
| `sdks/kotlin/rocky-room` | `buildOnRocky`, `setRockyDatabase`, `followRocky`, `settledOnRocky` and the pinned profile name |
| `crates/rocky-sdk`, `sdks/native` | Driver connections, the profile's housekeeping answers, capture and table events |

Applications own their entities, DAOs and queries. Nothing in these modules
depends on an application or on Compose.

## Entities

Every entity carries Rocky's identity as two non-null integer fields:

- `id`, the row's incarnation, which Rocky issues. It may be the primary key.
  With a natural key, the entity also declares the identity index Rocky
  creates: `@Index(value = ["id"], unique = true, name = "<table>_identity")`.
- `row_kind`, 0 for application rows; other kinds are reserved.

A `@ForeignKey` names the parent's identity: `parentColumns = ["id"]`, or a
partitioned parent's partition columns then `id`, such as `["list", "id"]`
with `childColumns = ["list", "category"]` in the
[shopping example](../../examples/shoppinglist-app/shared/src/data/db/entity/Category.kt).

`autoGenerate = true` is refused: Room never assigns a row id. Importing the
export refuses an entity without both fields, with `autoGenerate`, or with a
missing or misnamed identity index, naming the entity
([Room import](schema-definitions.md#import-a-room-export)); Room's own `CREATE TABLE` is refused
the same way by the [profile](sql-driver.md#room-284).

A DAO obtains an id before the insert, in the transaction that inserts it:
`SELECT rocky_next_id('<table>')` in a `@Query`, called from a `@Transaction`
method or inside `useWriterConnection { immediateTransaction { } }`
([row ids](sql-driver.md#row-ids)). Room's query verifier does not know the
function, hence `@SkipQueryVerification`. Outside a write transaction the
query fails; a rollback releases the id; an id Rocky did not issue fails the
statement or the commit. Whole-entity `@Update` and `@Upsert` of an existing
row write its unchanged `id`, which is allowed; `REPLACE` of a natural key
inserts a new `id` and deletes the old row.

Ids are signed under the device's author for the table: a table pseudonym an
issuer assigns once per device and table (`credentials.pseudonymRequest`,
`assignPseudonym`, `installPseudonym`, see
[author pseudonyms](credential-primitives.md#author-pseudonyms));
reopening restores it. Kotlin `create` and `join` install writable tables'
authors automatically. An issuer assigns its own; a member asks an issuer
over the renewal protocol. `installAuthors()` explicitly retries missing or
expired assignments, and member sync retries them too
([SDK enrollment](sdk-enrollment.md#author-installation)).

An entity whose table the schema marks
[command-controlled](confirmed-tables.md) is read-only: its DAO reads the
rows confirmed commands produced, and each write fails with code 23 and
publishes nothing. Submit a command instead.

## What a DAO write publishes

Every `@Insert`, `@Update`, `@Delete`, `@Upsert`, writing `@Query`,
`@Transaction` method and `useWriterConnection { immediateTransaction { } }`
runs in one Room transaction, which commits once and publishes one bundle.
Peers apply it atomically. Nested transactions are savepoints. Capture is in
the engine below the driver; a statement it cannot replicate fails with a
SQLite error code instead of writing silently
([codes](sql-driver.md#values-and-errors)).

- A bundle holds at most 2,048 rows and 4 MiB. A larger transaction fails
  with code 18 and publishes nothing; it is never split.
- A bundle records the net change: a row inserted and deleted in one
  transaction publishes nothing.
- Under the `changed` update policy only columns whose values differ are
  published, so a whole-entity `@Update` keeps another device's concurrent
  change of another column.
- `SET n = n + 1` publishes the computed value, not an increment.
- An open write transaction holds the replica for at most 5 s by default,
  then fails with code 4 until Room rolls back. Readers never delay it.
- A commit answers before the device flush
  ([durability](sql-driver.md#durability)); [local latency](local-latency.md)
  measures a DAO write and its observer.

## Observation

Room refreshes its observers after its own transactions. Sync writes,
rebuilds and authority changes never pass through Room's connections:
`followRocky` refreshes Room's invalidation tracker after every committed
[table event](queries-and-observation.md#table-events). Run it for the life of
the Room database and cancel it before closing Room. Without it, DAO flows
miss synced rows until the next local write. `buildOnRocky` owns this follower
and cancels it when its `RockyRoom` wrapper closes; Rocky remains open.

Room reads a flow's query inside a read transaction. That transaction fails,
and the flow ends, when an authority change such as an installed partition
secret ends its session, while sync applies a bundle that arrived in part
(`held partition is incomplete`), while a scope is unavailable, or when its
read lease expires and rolls back the transaction. Collect
such flows with `settledOnRocky()`, which uses a bounded backoff for these
transient read failures and lets other errors end the flow. Retrying a rolled-back
read queries committed rows again; it never resumes the expired transaction.
A virtual-clock qualification injects that timeout and a permanent storage
failure, checking recovery and error propagation. The
[shopping example](../../examples/shoppinglist-app/shared/src/data/ShoppingRepository.kt)
uses this retry for its observations.

## Schema and migrations

Rocky creates the tables when the database is created or joined, with real
primary keys and indexes; ordinary SQLite tools see them in the file. Room
finds the version it expects in `PRAGMA user_version`, which is the
replicated schema's version, and never runs `onCreate`. A Room migration or
destructive fallback fails at its first schema change: the replicated schema
changes through Rocky's [generations](generations.md), never through Room.
Imported tables exclude Room, Rocky and SQLite bookkeeping tables.

## Pairing

Enrollment, invitations, key storage, renewal, revocation and logout are in
[SDK enrollment](sdk-enrollment.md); `RockyDatabase` exposes them as
`pairing()`, `session()`, `devices()`, `revoke(node)`, `renew()` and
`logout(purge)`. A writer's grant names the Room tables it may write. An OS
lock prevents two native engines opening the same directory; do not clone a
data directory or its keys. Local rows are plaintext at rest.

## Build and verification

Every SDK Gradle build prepares SDK-owned bindings and the native resource;
that needs Rust, and consumers of a packaged artifact do not.

- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:room:jvmTest` is the acceptance reference for the
  pinned version. Room's recorded statements replay through the driver
  ([statement fixture](sql-driver.md#qualification)); the same DAOs run on
  Rocky's file with the ids Rocky issues, and an id Rocky did not issue is
  refused; DAO flows follow synced writes and miss them without `followRocky`;
  a `@Transaction` of a parent and its children is one bundle and one table
  event on a peer; 2,049 rows fail with code 18 and publish nothing; an insert
  and delete in one transaction publish nothing; concurrent whole-entity
  updates of different columns converge; a writer commits inside an open read
  transaction, whose snapshot holds; and ordinary SQLite opens the file
  read-only and shows the primary key, the index and a query plan using it.
  `RoomConstraintsTest` and `ConfirmedRowsTest` cover
  [constraints](mergeable-constraints.md#qualification) and
  [confirmed tables](confirmed-tables.md).
- `./examples/shoppinglist-app/check` runs the shopping repository on two
  devices: offline DAO edits converge field by field, a delete beats a
  concurrent rename, and a filtered DAO flow follows synced and local writes.
- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:check` checks that the example's hand declaration
  equals its imported Room export.

Limits: macOS ARM64 desktop; Android as recorded in [Android](android.md).
Room versions other than 2.8.4 need a new profile and fixture. The debug
native build steps about 2.7 ms per single-row DAO insert on the JVM, so a
transaction near the 2,048-row bundle limit needs a longer lease than 5 s
(`RockySQLiteDriver(database, ROOM_PROFILE, transactionTimeoutMillis)`);
release builds are about ten times faster.
