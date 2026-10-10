package org.example.examples.shopping

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import rocky.*
import rocky.room.settledOnRocky

/** A request for approval as a device sees it. [requester] and [decider] are device names where known. */
data class Approval(val key: String, val title: String, val note: String, val requester: String, val approved: Boolean?, val decider: String?)

/** Where this device stands with the coordination service. */
sealed interface Coordinated {
    /** Opened without a coordinator, such as over direct TCP: requests cannot be ordered. */
    data object Unavailable : Coordinated

    /** A trusted device waits for the household's tenant, or another device's identity, to sync. */
    data class Waiting(val reason: String) : Coordinated
    data class Registered(val revision: Long, val executors: Int, val invokers: Int, val expiresAtEpochSeconds: Long) : Coordinated

    /** A member: it asks, and a trusted device executes. */
    data object Member : Coordinated
    data class Failed(val message: String) : Coordinated
}

/**
 * Approvals as strong commands: any device asks with [AskApproval]; trusted devices decide with [DecideApproval], and
 * the coordination service orders both, so the first decision on a request stands everywhere. Trusted devices keep
 * the household's domain registered: every live device may invoke, every trusted one may execute.
 * See `examples/shoppinglist-app/README.md#approvals`.
 */
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
    private val wake = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    /** Revisions moved on after refusals in a row; bounded, so a refusing service is not asked forever. */
    private var refusals = 0

    fun requests(): Flow<List<Approval>> {
        val names = if (rocky.isIssuer) {
            household.registry().map { devices ->
                devices.mapNotNull { device -> device.command?.let { it to device.node } }.toMap()
            }
        } else {
            flowOf(emptyMap())
        }
        val devices = if (rocky.isIssuer) household.devices() else flowOf(emptyList())
        return combine(database.approvals().requests(), database.approvals().decisions(), names, devices) { requests, decisions, keys, rows ->
            val own = rocky.commandIdentity().node
            fun name(commandNode: String): String {
                if (!rocky.isIssuer) return if (commandNode == own) "This device" else "Another device"
                val node = if (commandNode == own) self else keys[commandNode]
                return rows.firstOrNull { it.node == node }?.name ?: "Another device"
            }
            requests.map { request ->
                val decision = decisions.firstOrNull { it.key == request.key }
                Approval(request.key, request.title, request.note, name(request.requester), decision?.approved, decision?.let { name(it.decider) })
            }
        }.settledOnRocky()
    }

    /** Asks the household to approve [title]; null once confirmed, else why it was not. */
    suspend fun ask(title: String, note: String): String? {
        require(title.isNotBlank())
        return settle(AskApproval, Ask(RockyDatabase.newHandle(), title.trim(), note.trim()))
    }

    /** On a trusted device: approves or declines; null once this decision stands, else why it does not. */
    suspend fun decide(key: String, approve: Boolean): String? = settle(DecideApproval, Decide(key, approve))

    private suspend fun <I> settle(command: Command<I, Unit, Refusal>, arguments: I): String? {
        check(coordinated) { "This device has no coordination service" }
        val handle = RockyDatabase.newHandle()
        rocky.submit(command, arguments, handle)
        val ticket = rocky.run(command, handle)
        // A ticket that did not settle stays stored; the background pass runs it again.
        wake.tryEmit(Unit)
        return when (val outcome = ticket.outcome) {
            is Outcome.Accepted -> null

            is Outcome.Rejected -> when (outcome.value.reason) {
                "approved" -> "Another device already approved it"
                "declined" -> "Another device already declined it"
                "missing" -> "The request has not reached the executor yet"
                else -> "Refused: ${outcome.value.reason}"
            }

            null -> ticket.error?.let { "Not settled yet (${it.code}): ${it.message}; it is retried in the background" }
                ?: "Sent; the result arrives with the next sync"
        }
    }

    /** Runs until cancelled: keeps the domain registered and retries tickets. */
    suspend fun run() {
        if (!coordinated) return
        coroutineScope {
            launch { retryTickets() }
            if (rocky.isIssuer) launch { register() }
        }
    }

    private suspend fun retryTickets() {
        while (true) {
            val unresolved = attempt { rocky.runCommands() } ?: 1
            // Settled identities free this device's response slots; the runtime retires only those still verifiable.
            if (unresolved == 0) attempt { rocky.acknowledgeCommands() }
            withTimeoutOrNull(if (unresolved > 0) 10_000L else 60_000L) { wake.first() }
        }
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
            val own = rocky.commandIdentity()
            val live = devices.filter { it.command != null }.associate { it.node to it.commandIdentity(own).encoded } + (self to own.encoded)
            val executors = devices.filter { it.profile == GrantProfile.Issuer }.map { it.node }.toSet() + self
            val members = Members(live.keys.filter { it in executors }.sorted().toSet(), live.keys.sorted().toSet())
            val now = epochMillis() / 1000
            when {
                // The founder's first revision names its members once there are any; nothing was sent yet.
                !row.applied && row.revision == 1L && row.members.isEmpty() -> attempt {
                    household.trusted.updateCoordination(1, row.control, members.toString(), 0, false)
                }

                !row.applied -> apply(row, Members.parse(row.members), live)

                row.members != members.toString() || row.expires - now < RENEW_SECONDS -> attempt {
                    household.trusted.updateCoordination(row.revision + 1, RockyDatabase.newHandle(), members.toString(), 0, false)
                }

                else -> mutableStatus.value = Coordinated.Registered(row.revision, members.executors.size, members.invokers.size, row.expires)
            }
        }
    }

    /** Sends the stored revision until the service answers; a refused control moves on to the next revision. */
    private suspend fun apply(row: CoordinationRow, members: Members, identities: Map<String, String>) {
        fun of(nodes: Set<String>) = identities.filterKeys { it in nodes }.values.map(CommandIdentity::decode)
        val owner = rocky.service(row.tenant)
        while (true) {
            try {
                if (row.revision == 1L) {
                    owner.register(row.registration, of(members.executors), of(members.invokers), LIFETIME_SECONDS)
                } else {
                    owner.control(row.revision, row.control, of(members.executors), of(members.invokers), LIFETIME_SECONDS)
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

    /** Who may execute and who may invoke, as stored in the `coordination` row: `executors;invokers`. */
    private data class Members(val executors: Set<String>, val invokers: Set<String>) {
        override fun toString() = executors.joinToString(",") + ";" + invokers.joinToString(",")
        companion object {
            fun parse(text: String) = text.split(";").let { parts ->
                Members(parts[0].split(",").filter { it.isNotEmpty() }.toSet(), parts.getOrElse(1) { "" }.split(",").filter { it.isNotEmpty() }.toSet())
            }
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
