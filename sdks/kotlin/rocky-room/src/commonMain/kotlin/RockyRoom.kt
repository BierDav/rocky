package rocky.room

import androidx.room.RoomDatabase
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.retryWhen
import rocky.RockyBinding
import rocky.RockyDatabase
import rocky.sqlite.RockySQLiteDriver
import kotlin.coroutines.CoroutineContext

/** The Room version whose statements the driver answers; see `docs/runtime/room-integration.md`. */
const val ROOM_PROFILE = "room-2.8.4"

/**
 * Room on Rocky's own file: every DAO reads and writes the typed tables of [database], one Room transaction is one
 * commit and one signed bundle. [queryContext] defaults to IO; include an exception handler for Room background
 * failures. Call [followRocky] so that sync writes reach DAO
 * observers, or use [buildOnRocky], which does both.
 */
fun <T : RoomDatabase> RoomDatabase.Builder<T>.setRockyDatabase(database: RockyDatabase, queryContext: CoroutineContext = Dispatchers.IO): RoomDatabase.Builder<T> = setDriver(RockySQLiteDriver(database, ROOM_PROFILE)).setQueryCoroutineContext(queryContext)

/**
 * Room on a sealed state of [database]: the same DAOs read a seal this device keeps, or the seal a bookmark shows,
 * through views shaped like the live tables. Writes fail with SQLite's read-only code 8, and nothing changes the
 * state, so the database needs no [followRocky]. See `docs/runtime/kept-seals.md`.
 */
fun <T : RoomDatabase> RoomDatabase.Builder<T>.setRockySeal(database: RockyDatabase, binding: RockyBinding, queryContext: CoroutineContext = Dispatchers.IO): RoomDatabase.Builder<T> = setDriver(RockySQLiteDriver(database, ROOM_PROFILE, binding = binding)).setQueryCoroutineContext(queryContext)

/** Builds Room on [database]'s file and follows its sync in [queryContext] until [RockyRoom.close]. */
fun <T : RoomDatabase> RoomDatabase.Builder<T>.buildOnRocky(database: RockyDatabase, queryContext: CoroutineContext = Dispatchers.IO): RockyRoom<T> = RockyRoom(setRockyDatabase(database, queryContext).build(), database, queryContext)

/**
 * A Room database on Rocky's file whose observers see sync writes. Closing it closes Room, not [rocky]: the
 * application closes that after.
 */
class RockyRoom<T : RoomDatabase> internal constructor(val room: T, val rocky: RockyDatabase, queryContext: CoroutineContext) {
    private val following = CoroutineScope(queryContext + SupervisorJob()).launch { room.followRocky(rocky) }
    suspend fun close() = withContext(NonCancellable) {
        following.cancelAndJoin()
        room.close()
    }
}

/**
 * Refreshes Room's invalidation tracker after every committed change of [database]'s tables: sync, rebuilds and
 * authority changes, which no trigger on Room's connections sees. Runs until cancelled; cancel it before closing Room.
 */
suspend fun RoomDatabase.followRocky(database: RockyDatabase) {
    // Registered before the first refresh, so no commit falls between them.
    val events = database.driverTableEvents()
    invalidationTracker.refreshAsync()
    try {
        while (currentCoroutineContext().isActive) {
            events.next()
            invalidationTracker.refreshAsync()
        }
    } finally {
        events.destroy()
    }
}

/** Moves through Rocky's atomic write surface and refreshes this Room projection after commit. */
suspend fun RoomDatabase.moveRockyRow(
    database: RockyDatabase,
    handle: String,
    table: String,
    id: Long,
    from: List<Any?>,
    to: List<Any?>,
) {
    database.moveRow(handle, table, id, from, to)
    invalidationTracker.refreshAsync()
}

/**
 * Observes a DAO flow again, with a short backoff, when its read transaction met a transient state: an authority
 * change such as a newly installed partition secret, a bundle sync applies in part, or a partition that is not
 * available or held yet, or a read lease timeout. A screen stays on the last committed rows instead of ending. Other failures end the flow.
 */
fun <T> Flow<T>.settledOnRocky(): Flow<T> = retryWhen { cause, attempt ->
    val message = cause.message.orEmpty()
    val transient = TRANSIENT_READS.any { it in message }
    if (transient) delay(minOf(50L shl minOf(attempt, 4L).toInt(), 1_000L))
    transient
}

// A list's row may arrive before its other tables are held, while its key installs.
private val TRANSIENT_READS = listOf("held partition is incomplete", "session authority changed", " is not available: ", "named partition is not held", "the workspace waits for ", "SQL session: transaction was rolled back: Timeout")
