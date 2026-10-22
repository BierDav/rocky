package org.example.examples.shopping.data.tips

import androidx.room.RoomDatabase
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingTables
import org.example.examples.shopping.util.hex
import org.example.examples.shopping.util.randomBytes
import rocky.*
import rocky.room.setRockySeal

/** The bookmark every household device follows unless it chose a branch. */
const val MAIN = "main"

/**
 * Seals, kept seals, branches and held changes as the household uses them; see `docs/runtime/shopping-seals.md`.
 * Rocky runs no timer: [maintain] is the app's policy, run by the node's upkeep and after each switch.
 */
class HouseholdTips(
    private val rocky: RockyDatabase,
    /** Builds Room for a read-only view; the path names only its lock file. */
    private val room: (path: String) -> RoomDatabase.Builder<ShoppingDatabase>,
    private val directory: String,
    /** Writes gathered on `main` before a trusted device seals it. */
    private val sealAfter: ULong = SEAL_AFTER,
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow<TipsState?>(null)

    /** The latest [refresh]ed state; null until the first one. */
    val state: StateFlow<TipsState?> = mutableState.asStateFlow()

    /** Why the last policy round failed, until one succeeds. */
    val problem = MutableStateFlow<String?>(null)

    /** A bookmark this device asked to follow whose starting seal it is still pulling from a peer. */
    private var pendingFollow: String? = null

    /**
     * One round of the app's policy: trusted devices keep every seal, and seal `main` once [sealAfter] writes
     * gathered since the last seal; every device compacts, leaving [KEEP] seals uncompacted unless one is final.
     * A pending branch switch is retried. Expected refusals, such as a seal that lost, wait for the next round.
     */
    suspend fun maintain(): Maintenance = lock.withLock {
        keepSeals()
        pendingFollow?.let { name -> if (followHeld(name)) pendingFollow = null }
        val status = rocky.tips.status()
        var sealed: String? = null
        if (rocky.isIssuer && status.bookmark == MAIN && status.operationsSinceSeal >= sealAfter) {
            sealed = expected { rocky.tips.seal().tip }
        }
        val compaction = rocky.tips.compact(KEEP)
        Maintenance(sealed, compaction).also { refreshLocked() }
    }

    /**
     * Called at open. Trusted devices keep every seal, others none; a seal made before is kept only while the live
     * tables still hold its state. Members release list history at compaction once another device acknowledged it,
     * and a device that missed it imports the base at a seal. Trusted devices keep their history: a migration or a
     * rollback closes a tip, and a close needs every author position this device holds.
     */
    suspend fun keepSeals() {
        val kept = if (rocky.isIssuer) KeptSeals(every = true) else KeptSeals()
        if (rocky.tips.kept() != kept) rocky.tips.keep(kept)
        val window = if (rocky.isIssuer) KEPT_HISTORY else 0L
        for (table in ShoppingTables) {
            val current = rocky.retention(table).getValue("policy").jsonObject
            if (current["window"]?.jsonPrimitive?.long != window) rocky.retention(table, JsonObject(current + ("window" to JsonPrimitive(window))))
        }
    }

    /** Seals the followed tip now, whatever the policy; its bookmark moves to the successor. */
    suspend fun seal(): TipStatus = lock.withLock { rocky.tips.seal().also { refreshLocked() } }

    suspend fun refresh(): TipsState = lock.withLock { refreshLocked() }

    private suspend fun refreshLocked(): TipsState {
        val status = rocky.tips.status()
        val inspection = rocky.tips.inspect()
        val path = path(inspection, status.tip)
        return TipsState(
            bookmark = status.bookmark,
            tip = status.tip,
            operationsSinceSeal = status.operationsSinceSeal,
            // A deleted bookmark stays in the graph without a tip.
            bookmarks = inspection.bookmarks.filter { it.tip != null }.map { Bookmark(it.name, it.tip, it.name == status.bookmark) },
            losers = inspection.bookmarks.flatMap { bookmark -> bookmark.losers.map { Loser(it.name, it.tip) } },
            path = path,
            compaction = rocky.tips.compaction(),
            held = rocky.heldChanges().changes,
            pendingFollow = pendingFollow,
        ).also { mutableState.value = it }
    }

    /**
     * Follows [name]. Following a branch whose starting seal this device does not hold asks a peer that keeps it
     * and switches during a later [maintain]; the result says which happened. Writes made on the bookmark left wait
     * there until this device returns.
     */
    suspend fun follow(name: String): Follow = lock.withLock {
        pendingFollow = null
        val outcome = if (followHeld(name)) {
            switched()
        } else {
            pendingFollow = name
            Follow.Pulling
        }
        refreshLocked()
        outcome
    }

    /** Where a switch stands: a bookmark with another schema switches at the end of the next sync pass. */
    suspend fun switched(): Follow {
        val generation = rocky.generation()
        return when {
            generation["pending"] !is JsonNull && generation["pending"] != null -> Follow.Pending
            generation["application"]?.jsonPrimitive?.content == "ahead" -> Follow.Ahead
            else -> Follow.Switched
        }
    }

    /** True once followed; false when the seal its tip starts from must be pulled first, which this asks for. */
    private suspend fun followHeld(name: String): Boolean = try {
        rocky.tips.follow(name)
        true
    } catch (error: RockyException) {
        val start = startingSeal(rocky.tips.inspect(), name) ?: throw error
        if (error.code != "rejected" || start.held != null) throw error
        rocky.tips.pull(start.id)
        false
    }

    /**
     * Opens a branch named [name] on the latest seal of the followed tip's path, sealing the tip first when nothing
     * is sealed yet, and follows it. Only the owner and trusted identities open branches; Rocky refuses others.
     */
    suspend fun branch(name: String): String = lock.withLock {
        require(name.isNotBlank() && name != MAIN) { "Choose a branch name other than $MAIN" }
        keepSeals()
        var status = rocky.tips.status()
        if (path(rocky.tips.inspect(), status.tip).isEmpty()) status = rocky.tips.seal()
        // Opening finalizes the path first; it refuses a seal whose state this device does not hold.
        val tip = rocky.tips.open(path(rocky.tips.inspect(), status.tip).first().id, BRANCH_MINIMUM)
        rocky.tips.createBookmark(name, tip)
        rocky.tips.follow(name)
        refreshLocked()
        tip
    }

    /**
     * Returns to `main`. A member releases [name]'s tip on this device, once a peer reported holding its own writes
     * there. With [delete], a device allowed to deletes the bookmark for the household instead and keeps the tip's
     * frames: its writes to trusted-only tables there reached no other device.
     */
    suspend fun leave(name: String, delete: Boolean = false) = lock.withLock {
        require(name != MAIN) { "$MAIN is the household's bookmark" }
        val status = rocky.tips.status()
        // A bookmark another device deleted has no tip; the one this device followed is still known.
        val tip = rocky.tips.inspect().bookmarks.singleOrNull { it.name == name }?.tip ?: status.tip.takeIf { status.bookmark == name }
        if (status.bookmark == name) rocky.tips.follow(MAIN)
        if (delete) rocky.tips.deleteBookmark(name) else tip?.let { rocky.tips.drop(it) }
        refreshLocked()
    }

    /** Points `main` back at a new tip opened on [seal], which this device holds. Only the owner and trusted identities may. */
    suspend fun rollBack(seal: String): String = lock.withLock {
        val tip = rocky.tips.open(seal, BRANCH_MINIMUM)
        rocky.tips.moveBookmark(MAIN, tip)
        refreshLocked()
        tip
    }

    /** The lists and their items as a seal this device holds showed them, read through a read-only view. */
    suspend fun snapshot(seal: String): List<ListSnapshot> {
        val view = room("$directory/view-${randomBytes(6).hex()}").setRockySeal(rocky, RockyBinding.Seal(seal)).build()
        try {
            // A bound view never changes, so it is read once; Room's change tracking needs a writable connection.
            return view.lists().all().map { list ->
                ListSnapshot(list.id, list.title, view.items().all(list.id).map { HeldItem(it.title, it.completed) })
            }
        } finally {
            view.close()
        }
    }

    /**
     * Re-authors held change [id] as new writes of today's schema: inserts get fresh ids, updates and deletes
     * name their row. Rows of tables the schema removed are left out.
     */
    suspend fun reauthor(id: String) = lock.withLock {
        val change = rocky.heldChanges().changes.single { it.id == id }
        rocky.reauthor(id) { change.rows.filterNot { it.removed }.forEach { reauthorRow(it) } }
        refreshLocked()
    }

    suspend fun dropHeld(id: String) = lock.withLock {
        rocky.dropHeld(id)
        refreshLocked()
    }

    private suspend fun SqlWrite.reauthorRow(row: HeldRow) {
        val kept = row.fields.filter { !it.removed }
        val partition = kept.singleOrNull { it.name == "list" }?.value
        val key = kept.singleOrNull { it.name == "id" }?.value ?: row.row.toLongOrNull()
        val where = if (partition != null) "list = ? AND id = ?" to listOf(partition, key) else "id = ?" to listOf(key)
        when (row.kind) {
            HeldKind.Insert -> {
                val fields = kept.filter { it.written && it.name != "id" }
                val fresh = if (partition != null) nextId(row.table, partition) else nextId(row.table)
                val columns = fields.map { it.name } + "id"
                execute("INSERT INTO ${row.table}(${columns.joinToString()}) VALUES(${columns.joinToString { "?" }})", *(fields.map { it.value } + fresh).toTypedArray())
            }

            HeldKind.Update -> {
                val fields = kept.filter { it.written && it.name !in setOf("id", "list") }
                if (fields.isNotEmpty()) {
                    execute("UPDATE ${row.table} SET ${fields.joinToString { "${it.name} = ?" }} WHERE ${where.first}", *(fields.map { it.value } + where.second).toTypedArray())
                }
            }

            HeldKind.Delete -> execute("DELETE FROM ${row.table} WHERE ${where.first}", *where.second.toTypedArray())
        }
    }

    private suspend fun <T> expected(block: suspend () -> T): T? = try {
        block()
    } catch (error: RockyException) {
        if (error.code in EXPECTED) null else throw error
    }

    companion object {
        /** Writes gathered on `main` before a trusted device seals it. */
        const val SEAL_AFTER = 100uL

        /** Seals each device leaves uncompacted, unless a later one is final, so a lost seal rarely rebuilds. */
        const val KEEP = 1u

        /** A retention window no household reaches: trusted devices prune no list history. */
        const val KEPT_HISTORY = 1L shl 40

        /** The household's trusted devices run a branch's coordinated log; one in sync is enough. */
        const val BRANCH_MINIMUM = 1u

        /** Another seal ended the tip first, or the coordinator did not answer: the next round decides again. */
        private val EXPECTED = setOf("sealed", "coordinator_unavailable")
    }
}

/** The seals of the followed tip's path, newest first, whether reached by `continue` seals or by openings. */
internal fun path(inspection: TipInspection, tip: String): List<SealOnPath> {
    val seals = inspection.seals.filter { it.canonical && !it.revoked }
    val result = mutableListOf<SealOnPath>()
    var current = tip
    while (true) {
        val seal = seals.firstOrNull { it.id == current }
            ?: inspection.openings.firstOrNull { it.id == current }?.let { opening -> seals.firstOrNull { it.id == opening.seal } }
            ?: break
        result += SealOnPath(seal.id, seal.tip, seal.held, seal.final, seal.action is TipSealAction.Close)
        current = seal.tip
    }
    return result
}

/** The seal [bookmark]'s tip starts from. */
private fun startingSeal(inspection: TipInspection, bookmark: String): SealOnPath? {
    val tip = inspection.bookmarks.singleOrNull { it.name == bookmark }?.tip ?: return null
    return path(inspection, tip).firstOrNull()
}

data class TipsState(
    val bookmark: String?,
    val tip: String,
    val operationsSinceSeal: ULong,
    val bookmarks: List<Bookmark>,
    /** Statements that lost a race, such as `main/1`; their tips stay branches. */
    val losers: List<Loser>,
    val path: List<SealOnPath>,
    val compaction: TipCompaction,
    val held: List<HeldChange>,
    val pendingFollow: String?,
)

data class Bookmark(val name: String, val tip: String?, val followed: Boolean)
data class Loser(val name: String, val tip: String?)

/** [held] says whether a read-only view can show it; [closed] seals end a tip, as a schema migration does. */
data class SealOnPath(val id: String, val tip: String, val held: TipHeld?, val final: Boolean, val closed: Boolean)

data class ListSnapshot(val id: Long, val title: String, val items: List<HeldItem>)
data class HeldItem(val title: String, val completed: Boolean)

/** What one [HouseholdTips.maintain] round did. */
data class Maintenance(val sealed: String?, val compaction: TipCompaction)

enum class Follow {
    Switched,

    /** The bookmark has another schema; the next sync pass switches the database to it. */
    Pending,

    /** The bookmark's schema is older than this app's: reopening publishes this app's schema there. */
    Ahead,

    /** The branch's starting state is being pulled from a device that keeps it. */
    Pulling,
}
