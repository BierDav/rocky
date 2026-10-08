package rocky.room

import androidx.room.RoomDatabase
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import rocky.RockyDatabase
import rocky.sqlite.RockySQLiteDriver

/** The Room version whose statements the driver answers; see `docs/runtime/room-integration.md`. */
const val ROOM_PROFILE = "room-2.8.4"

/**
 * Room on Rocky's own file: every DAO reads and writes the typed tables of [database], one Room transaction is one
 * commit and one signed bundle. Queries run on the IO dispatcher. Call [followRocky] so that sync writes reach DAO
 * observers.
 */
fun <T : RoomDatabase> RoomDatabase.Builder<T>.setRockyDatabase(database: RockyDatabase): RoomDatabase.Builder<T> =
    setDriver(RockySQLiteDriver(database, ROOM_PROFILE)).setQueryCoroutineContext(Dispatchers.IO)

/**
 * Refreshes Room's invalidation tracker after every committed change of [database]'s tables: sync, rebuilds and
 * authority changes, which no trigger on Room's connections sees. Runs until cancelled; cancel it before closing Room.
 */
suspend fun RoomDatabase.followRocky(database: RockyDatabase) {
    // Registered before the first refresh, so no commit falls between them.
    val events = database.driverTableEvents()
    invalidationTracker.refreshAsync()
    while (currentCoroutineContext().isActive) {
        events.next()
        invalidationTracker.refreshAsync()
    }
}
