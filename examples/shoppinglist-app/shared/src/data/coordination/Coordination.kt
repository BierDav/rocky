package org.example.examples.shopping.data.coordination

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*
import org.example.examples.shopping.data.db.CoordinationRow
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.TrustedRows
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.util.epochMillis
import org.example.examples.shopping.util.randomBytes
import rocky.*
import rocky.room.settledOnRocky

/** A request for approval as a device sees it. [requester] and [decider] are device names where known. */
data class Approval(val key: String, val title: String, val note: String, val requester: String, val approved: Boolean?, val decider: String?)

/** Where this device stands with the coordination service. */
sealed interface Coordinated {
    /** Opened without a coordinator, such as over direct TCP: decisions are unavailable. */
    data object Unavailable : Coordinated

    /** A trusted device waits for the household's tenant, or another device's identity, to sync. */
    data class Waiting(val reason: String) : Coordinated
    data class Registered(val revision: Long, val trusted: Int, val expiresAtEpochSeconds: Long) : Coordinated

    /** A member writes requests and reads committed decisions. */
    data object Member : Coordinated
    data class Failed(val message: String) : Coordinated
}

/** Ordinary request rows and coordinated decisions; the first committed decision stands. */
class Approvals internal constructor(
    private val database: ShoppingDatabase,
    private val rocky: RockyDatabase,
    private val household: Household,
    private val self: String,
    val coordinated: Boolean,
) {
    private val mutableStatus = MutableStateFlow<Coordinated>(
        if (!coordinated) {
            Coordinated.Unavailable
        } else if (rocky.isIssuer) {
            Coordinated.Waiting("Starting")
        } else {
            Coordinated.Member
        },
    )
    val status: StateFlow<Coordinated> = mutableStatus.asStateFlow()

    /** Revisions moved on after refusals in a row; bounded, so a refusing service is not asked forever. */
    private var refusals = 0

    fun requests(): Flow<List<Approval>> {
        val devices = if (rocky.isIssuer) household.devices() else flowOf(emptyList())
        return combine(database.approvals().requests(), database.approvals().decisions(), devices) { requests, decisions, rows ->
            fun name(node: String): String = if (node == self) "This device" else rows.firstOrNull { it.node == node }?.name ?: "Another device"
            requests.map { request ->
                val decision = decisions.firstOrNull { it.key == request.key }
                Approval(request.key, request.title, request.note, name(request.requester), decision?.approved, decision?.let { name(it.decider) })
            }
        }.settledOnRocky()
    }

    /** Asking is an ordinary write, including while offline. */
    suspend fun ask(title: String, note: String): String? {
        require(title.isNotBlank())
        rocky.write {
            execute("INSERT INTO requests(key,title,note,requester,id) VALUES(?,?,?,?,?)", RockyDatabase.newHandle(), title.trim(), note.trim(), self, nextId("requests"))
        }
        return null
    }

    private class Unchanged(val reason: String) : Exception(reason)

    /** Decides once at the committed log head; a competing writer retries after catching up. */
    suspend fun decide(key: String, approve: Boolean): String? {
        check(coordinated && rocky.isIssuer) { "Only a trusted device with coordination may decide" }
        repeat(16) {
            try {
                rocky.coordinated {
                    if (query("SELECT key FROM requests WHERE key=?", key).isEmpty()) throw Unchanged("The request has not reached this device yet")
                    val previous = query("SELECT approved FROM decisions WHERE key=?", key).singleOrNull()
                    if (previous != null) {
                        val approved = previous.jsonArray[0].jsonObject.getValue("value").jsonPrimitive.content.toLong() != 0L
                        throw Unchanged(if (approved) "Another device already approved it" else "Another device already declined it")
                    }
                    execute("INSERT INTO decisions(key,approved,decider,id) VALUES(?,?,?,?)", key, approve, self, nextId("decisions"))
                }
                return null
            } catch (unchanged: Unchanged) {
                return unchanged.reason
            } catch (failure: RockyException) {
                if (failure.code !in setOf("conflict", "behind", "aborted", "below_minimum")) throw failure
                delay(20)
            }
        }
        throw RockyException("conflict", true, "Another decision is still committing; try again")
    }

    /** Keeps the domain's trusted-node control registered until cancelled. */
    suspend fun run() {
        if (coordinated && rocky.isIssuer) register()
    }

    @OptIn(FlowPreview::class)
    private suspend fun register() {
        val renewals = flow {
            while (true) {
                emit(Unit)
                delay(60_000)
            }
        }
        combine(household.trusted.coordination(), household.registry(), renewals) { row, devices, _ ->
            row to devices.filter { !it.revoked && it.profile !is GrantProfile.Custody }
        }.settledOnRocky().debounce(1_000).collectLatest { (row, devices) ->
            if (row == null) {
                mutableStatus.value = Coordinated.Waiting("Waiting for the household's coordination key to sync")
                return@collectLatest
            }
            val members = Members((devices.filter { it.profile == GrantProfile.Issuer }.map { it.node } + self).sorted().toSet())
            val now = epochMillis() / 1000
            when {
                // The founder's first revision names its members once there are any; nothing was sent yet.
                !row.applied && row.revision == 1L && row.members.isEmpty() -> attempt {
                    household.trusted.updateCoordination(1, row.control, members.toString(), 0, false)
                }

                !row.applied -> apply(row, Members.parse(row.members))

                row.members != members.toString() || row.expires - now < RENEW_SECONDS -> attempt {
                    household.trusted.updateCoordination(row.revision + 1, RockyDatabase.newHandle(), members.toString(), 0, false)
                }

                else -> mutableStatus.value = Coordinated.Registered(row.revision, members.trusted.size, row.expires)
            }
        }
    }

    /** Sends the stored revision until the service answers; a refused control moves on to the next revision. */
    private suspend fun apply(row: CoordinationRow, members: Members) {
        val owner = rocky.service(row.tenant)
        while (true) {
            try {
                if (row.revision == 1L) {
                    owner.register(row.registration, members.trusted.toList(), LIFETIME_SECONDS)
                } else {
                    owner.control(row.revision, row.control, members.trusted.toList(), LIFETIME_SECONDS)
                }
                refusals = 0
                household.trusted.updateCoordination(row.revision, row.control, row.members, kotlin.time.Clock.System.now().toEpochMilliseconds() / 1000 + LIFETIME_SECONDS, true)
                return
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                mutableStatus.value = Coordinated.Failed("Coordination service: ${error.message ?: error}")
                // Another trusted device may have sent this revision first: the next one names the current members.
                if (error is RockyException && !error.retryable) {
                    if (row.revision > 1 && refusals++ < 3) attempt { household.trusted.updateCoordination(row.revision + 1, RockyDatabase.newHandle(), row.members, 0, false) }
                    return
                }
                delay(30_000)
            }
        }
    }

    /** Trusted node keys, stored in canonical sorted order. */
    private data class Members(val trusted: Set<String>) {
        override fun toString() = trusted.sorted().joinToString(",")
        companion object {
            fun parse(text: String) = Members(text.split(",").filter { it.isNotEmpty() }.toSet())
        }
    }

    private suspend fun <T> attempt(block: suspend () -> T): T? = try {
        block()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Exception) {
        null
    }

    companion object {
        /** The service's genesis allows controls of up to 30 days; renew a week before one ends. */
        private const val LIFETIME_SECONDS = 29L * 86_400
        private const val RENEW_SECONDS = 7L * 86_400

        /** On the founding device only: the household's tenant key and registration, applied once online. */
        internal suspend fun start(trusted: TrustedRows) = trusted.startCoordination(randomBytes(32), RockyDatabase.newHandle())
    }
}
