package org.example.examples.shopping

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import rocky.GrantProfile
import rocky.RockyDatabase
import rocky.RockyEvent
import rocky.room.settledOnRocky

/** A `devices` horizon: devices that have not acknowledged it, the latest time it waits for them, and whether it ended. */
data class Purge(val waiting: Int, val deadlineEpochSeconds: Long?, val finalized: Boolean)

/**
 * The household's devices and how list keys reach them, without a server: a trusted device records each device it
 * enrolls in `devices` and seals every list's key to trusted and full devices in `keys`, which sync like any row;
 * each device installs the keys sealed to it. Partial devices receive only lists shared with them by [share].
 */
class Household(private val database: ShoppingDatabase, private val rocky: RockyDatabase, val self: String) {
    private val notes = MutableSharedFlow<String>(extraBufferCapacity = 64)

    /** The trusted-only tables; a member never touches them. */
    internal val trusted = TrustedRows(rocky)

    private val invitationsLock = Mutex()

    /** Choices by native invitation event, including enrollments the collector has not processed yet. */
    private val pending = sortedMapOf<Long, PairingCode>()

    fun devices(): Flow<List<DeviceRow>> = database.devices().observe().settledOnRocky()

    /** Durable issuer state, refreshed when its device policy changes. */
    fun revokedDevices(): Flow<Set<String>> = if (!rocky.isIssuer) {
        flowOf(emptySet())
    } else {
        rocky.activity()
            .filter { it is RockyEvent.DeviceRevoked || it is RockyEvent.Renewed }
            .map { Unit }.onStart { emit(Unit) }
            .map { rocky.devices().filter { it.revoked }.map { it.node }.toSet() }
            .distinctUntilChanged()
    }

    /** Which device was sent the key of which list, at which epoch. */
    fun keys(): Flow<List<ListKey>> = database.keys().observe().settledOnRocky()

    /** What key delivery did, for the activity log. */
    val deliveries: SharedFlow<String> = notes.asSharedFlow()

    /** On a trusted device: a single-use code for one new device of [kind], replacing the previous one. */
    suspend fun invite(kind: NodeKind, name: String): PairingCode = invitationsLock.withLock {
        val invitation = rocky.pairing(kind.profile)
        val code = PairingCode(kind, name, invitation.uri)
        pending[invitation.sequence] = code
        if (pending.size > 512) pending.remove(pending.firstKey())
        // Other trusted devices list it too; members cannot read `invitations`.
        attempt("list the invitation") {
            trusted.openInvitation(self, kind.name.lowercase(), name, code.toString(), invitation.expiresAtEpochSeconds)
        }
        code
    }

    /** On a trusted device: each trusted device's open invitation, expired ones included. */
    fun invitations(): Flow<List<InvitationRow>> = if (!rocky.isIssuer) flowOf(emptyList()) else trusted.invitations()
    suspend fun record(node: String, name: String, kind: NodeKind) = database.devices().record(node, name, kind.name.lowercase())

    /** On a trusted device: seals [list]'s current key to the partial device [node]. */
    suspend fun share(list: Long, node: String) = deliver(list, listOf(node))

    /** Revokes future access and rotates the keys this device could read. */
    suspend fun revoke(node: String) {
        rocky.revoke(node)
    }

    /**
     * On a trusted device: removes a revoked device's record, the keys sealed to it and its identity in one commit, so
     * it disappears everywhere with the next sync. The signed revocation stays: it is what refuses the old identity.
     * Only the deleted rows' tombstones remain, until a horizon declared with [purge] is finalized.
     */
    suspend fun forget(node: String) {
        check(node in revokedDevices().first()) { "Only a revoked device can be removed" }
        database.devices().forget(node)
        runCatching { rocky.removePeer(node) }
    }

    /**
     * Declares horizons on `devices` and `identities` with every live device as participant: once each acknowledged
     * them, or reached its deadline, sync finalizes them and drops the removed rows' tombstones and history. Until then
     * both tables refuse writes, so enrollments are recorded afterwards. Fails until this device synced with each one.
     * Removed `keys` rows keep their history: those envelopes open only key epochs the revoked device already held.
     */
    suspend fun purge() {
        val participants = rocky.devices().filter { !it.revoked && it.node != self && it.profile !is GrantProfile.Custody }.map { it.node }.toSet()
        for (table in PURGED) rocky.horizons.declare(table, participants)
    }

    /** The latest purge, or null when none was declared: the later deadline, and finalized once both tables are. */
    suspend fun purgeStatus(): Purge? = PURGED.map { rocky.horizons.status(it) }
        .filter { it["statement"] != null && it["statement"] !is JsonNull }
        .map { status ->
            Purge(
                (status["waiting"] as? JsonArray)?.size ?: 0,
                status["deadline"]?.jsonPrimitive?.longOrNull,
                status["finalized"]?.jsonPrimitive?.boolean == true,
            )
        }
        .reduceOrNull { a, b ->
            Purge(maxOf(a.waiting, b.waiting), listOfNotNull(a.deadlineEpochSeconds, b.deadlineEpochSeconds).maxOrNull(), a.finalized && b.finalized)
        }

    /** Runs until cancelled: records enrollments and delivers keys on a trusted device, and installs its own keys. */
    suspend fun run() = coroutineScope {
        if (rocky.isIssuer) {
            launch { recordEnrollments() }
            launch { deliverEveryList() }
        }
        launch { installOwnKeys() }
    }

    private suspend fun recordEnrollments() {
        rocky.activity().filterIsInstance<RockyEvent.DeviceEnrolled>().collect { enrolled ->
            val known = devices().first().any { it.node == enrolled.node }
            val choice = invitationsLock.withLock {
                val sequence = pending.keys.lastOrNull { it < enrolled.sequence }
                sequence?.let { pending.remove(it) }?.also { code ->
                    attempt("close the invitation") { trusted.closeInvitation(self, code.toString()) }
                }
            }
            if (known) return@collect
            val (kind, name) = choice?.let { it.kind to it.name }
                ?: (if (enrolled.profile == GrantProfile.Issuer) NodeKind.Trusted else NodeKind.Partial) to "Device ${enrolled.node.take(6)}"
            // A pending `devices` horizon refuses the write until it is finalized: keep the enrollment until then.
            while (attempt("record $name") { record(enrolled.node, name, kind) } == null) delay(15_000)
            notes.tryEmit("Enrolled $name as ${kind.label.lowercase()}")
        }
    }

    @OptIn(FlowPreview::class)
    private suspend fun deliverEveryList() {
        combine(database.lists().observe(), devices(), database.keys().observe(), revokedDevices()) { lists, devices, keys, revoked ->
            Triple(lists, devices.filter { it.node !in revoked }, keys)
        }
            .settledOnRocky()
            .debounce(300)
            .collectLatest { (lists, devices, keys) ->
                val recipients = devices.filter { it.node != self && NodeKind.of(it.kind)?.receivesEveryList == true }
                // A partial device keeps receiving the new epoch of lists it was given.
                val given = keys.filter { key -> devices.any { it.node == key.recipient && NodeKind.of(it.kind) == NodeKind.Partial } }
                for (list in lists) {
                    val epoch = attempt("read ${list.title}") { rocky.list(list.id).status().epochs.maxOrNull()?.toString() } ?: continue
                    val stale = (recipients.map { it.node } + given.filter { it.list == list.id }.map { it.recipient })
                        .filter { node -> keys.none { it.recipient == node && it.list == list.id && it.epoch == epoch } }
                    for (recipient in stale) {
                        attempt("share ${list.title} with ${recipient.take(6)}") {
                            deliver(list.id, listOf(recipient))
                            notes.tryEmit("Sealed “${list.title}” to ${devices.firstOrNull { it.node == recipient }?.name ?: recipient.take(6)}")
                        }
                    }
                }
            }
    }

    private suspend fun installOwnKeys() {
        database.keys().observe().settledOnRocky().map { keys -> keys.filter { it.recipient == self } }.collect { keys ->
            for (key in keys) {
                val list = rocky.list(key.list)
                val held = attempt("read list ${key.list}") { list.status().epochs } ?: continue
                if (held.any { it.toString() == key.epoch }) continue
                attempt("install list ${key.list}") {
                    list.accept(key.envelope)
                    notes.tryEmit("Installed the key of list ${key.list}, epoch ${key.epoch}")
                }
            }
        }
    }

    private suspend fun deliver(list: Long, recipients: List<String>) {
        val epoch = rocky.list(list).status().epochs.maxOrNull()?.toString() ?: error("no key of list $list is held here")
        for ((node, envelope) in rocky.list(list).share(recipients)) database.keys().deliver(node, list, epoch, envelope)
    }

    private companion object {
        /** The tables whose rows removing a device deletes and a purge finalizes. */
        val PURGED = listOf("devices", "identities")
    }

    private suspend fun <T> attempt(what: String, block: suspend () -> T): T? = try {
        block()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Exception) {
        notes.tryEmit("Could not $what: ${error.message}")
        null
    }
}
