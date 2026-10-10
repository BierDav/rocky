package org.example.examples.shopping

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import rocky.Device
import rocky.GrantProfile
import rocky.RockyDatabase
import rocky.RockyEvent
import rocky.room.settledOnRocky

/**
 * The household's devices and how list keys reach them, without a server: a trusted device records each device it
 * enrolls in `devices` and shares every list with trusted and full devices. Sync carries each envelope to its device
 * alone, directly or through the custody peer, and that device installs it; no row records who holds which list.
 * Partial devices receive only lists shared with them by [share], and the new epochs of a revocation's rotation.
 */
class Household(private val database: ShoppingDatabase, private val rocky: RockyDatabase, val self: String) {
    private val notes = MutableSharedFlow<String>(extraBufferCapacity = 64)

    /** The trusted-only tables; a member never touches them. */
    internal val trusted = TrustedRows(rocky)

    private val invitationsLock = Mutex()

    /** Choices by native invitation event, including enrollments the collector has not processed yet. */
    private val pending = mutableMapOf<Long, PairingCode>()

    /** Registry access exists only on an issuer; members never query it or the trusted tables. */
    fun registry(): Flow<List<Device>> {
        check(rocky.isIssuer) { "Only trusted devices read the registry" }
        return flow {
            while (true) {
                emit(rocky.devices())
                delay(500)
            }
        }.distinctUntilChangedBy { devices -> devices.map { it.copy(label = null) to it.label?.toList() } }
    }

    fun devices(): Flow<List<DeviceRow>> {
        check(rocky.isIssuer) { "Only trusted devices read devices" }
        return combine(trusted.devices(), registry()) { rows, registry ->
            rows.filter { it.node == self } +
                registry.map { device ->
                    device.label?.let(::deviceLabel)?.copy(node = device.node)
                        ?: rows.firstOrNull { it.node == device.node }
                        ?: DeviceRow(device.node, "Device ${device.node.take(6)}", kind(device).name.lowercase())
                }
        }.settledOnRocky()
    }

    /** Durable issuer state, refreshed when its device policy changes. */
    fun revokedDevices(): Flow<Set<String>> = if (!rocky.isIssuer) {
        flowOf(emptySet())
    } else {
        registry().map { devices -> devices.filter { it.revoked }.map { it.node }.toSet() }.distinctUntilChanged()
    }

    /**
     * On a trusted device: the lists whose current key this device sent to each node, by node. Only the sharer knows
     * its deliveries; it refreshes whenever one moves.
     */
    fun sent(): Flow<Map<String, Set<Long>>> = combine(
        database.lists().observe().settledOnRocky(),
        rocky.activity().filter { it is RockyEvent.EnvelopesDelivered || it is RockyEvent.PartitionChanged || it is RockyEvent.Renewed }
            .map { Unit }.onStart { emit(Unit) },
    ) { lists, _ -> lists }
        .map { lists ->
            val sent = mutableMapOf<String, MutableSet<Long>>()
            for (list in lists) {
                val status = attempt("read ${list.title}") { rocky.list(list.id).status() } ?: continue
                for (recipient in status.recipients.filter { it.epoch == status.current }) {
                    sent.getOrPut(recipient.node) { mutableSetOf() } += list.id
                }
            }
            sent
        }
        .distinctUntilChanged()

    /** What key delivery did, for the activity log. */
    val deliveries: SharedFlow<String> = notes.asSharedFlow()

    /** On a trusted device: a single-use code for one new device of [kind], replacing the previous one. */
    suspend fun invite(kind: NodeKind, name: String): PairingCode = invitationsLock.withLock {
        label(name, kind)
        val invitation = rocky.pairing(kind.profile)
        val code = PairingCode(kind, name, invitation.uri)
        pending[invitation.sequence] = code
        if (pending.size > 512) pending.remove(pending.keys.minOrNull())
        // Other trusted devices list it too; members cannot read `invitations`.
        attempt("list the invitation") {
            trusted.openInvitation(self, kind.name.lowercase(), name, code.toString(), invitation.expiresAtEpochSeconds)
        }
        code
    }

    /** On a trusted device: each trusted device's open invitation, expired ones included. */
    fun invitations(): Flow<List<InvitationRow>> = if (!rocky.isIssuer) flowOf(emptyList()) else trusted.invitations()
    suspend fun record(node: String, name: String, kind: NodeKind) {
        check(rocky.isIssuer)
        val label = label(name, kind)
        if (node != self) rocky.setDeviceLabel(node, label)
        trusted.record(node, name, kind.name.lowercase())
    }

    /** A contact scanned here grants no rights and its label never enters replicated storage. */
    suspend fun shareWithContact(list: Long, code: String): String {
        val node = rocky.addPeer(code)
        share(list, node)
        return node
    }

    /** Seals the held list to [node]; members reach this through [shareWithContact]. */
    suspend fun share(list: Long, node: String) {
        rocky.list(list).share(listOf(node))
    }

    /** Revokes future access and rotates the keys this device could read. */
    suspend fun revoke(node: String) {
        rocky.revoke(node)
    }

    /**
     * On a trusted device: removes a revoked device's application description; its registry tombstone remains.
     * its pending envelopes were dropped at the revocation. The signed revocation stays: it is what refuses the old identity.
     */
    suspend fun forget(node: String) {
        check(node in revokedDevices().first()) { "Only a revoked device can be removed" }
        rocky.setDeviceLabel(node, byteArrayOf())
        trusted.forget(node)
        runCatching { rocky.removePeer(node) }
    }

    /** Runs until cancelled on a trusted device: records enrollments and shares every list; sync delivers the keys. */
    suspend fun run() = coroutineScope {
        if (rocky.isIssuer) {
            launch { recordEnrollments() }
            launch { deliverEveryList() }
        }
    }

    private suspend fun recordEnrollments() {
        rocky.activity().filterIsInstance<RockyEvent.DeviceEnrolled>().collect { enrolled ->
            val known = trusted.devices().first().any { it.node == enrolled.node }
            val choice = invitationsLock.withLock {
                val sequence = pending.keys.filter { it < enrolled.sequence }.maxOrNull()
                sequence?.let { pending.remove(it) }?.also { code ->
                    attempt("close the invitation") { trusted.closeInvitation(self, code.toString()) }
                }
            }
            if (known) return@collect
            val (kind, name) = choice?.let { it.kind to it.name }
                ?: (if (enrolled.profile == GrantProfile.Issuer) NodeKind.Trusted else NodeKind.Partial) to "Device ${enrolled.node.take(6)}"
            while (attempt("record $name") { record(enrolled.node, name, kind) } == null) delay(15_000)
            notes.tryEmit("Enrolled $name as ${kind.label.lowercase()}")
        }
    }

    /**
     * Shares each list's current epoch with every trusted and full device this device has not sent it to. A
     * revocation's rotation seals the new epoch to every device whose record reads the list, partial ones included.
     */
    @OptIn(FlowPreview::class)
    private suspend fun deliverEveryList() {
        combine(database.lists().observe(), devices(), revokedDevices()) { lists, devices, revoked ->
            lists to devices.filter { it.node !in revoked }
        }
            .settledOnRocky()
            .debounce(300)
            .collectLatest { (lists, devices) ->
                val recipients = devices.filter { it.node != self && NodeKind.of(it.kind)?.receivesEveryList == true }
                for (list in lists) {
                    val status = attempt("read ${list.title}") { rocky.list(list.id).status() } ?: continue
                    val current = status.current ?: continue
                    val stale = recipients.filter { device -> status.recipients.none { it.node == device.node && it.epoch == current } }
                    if (stale.isEmpty()) continue
                    attempt("share ${list.title}") {
                        rocky.list(list.id).share(stale.map { it.node })
                        notes.tryEmit("Sealed “${list.title}” to ${stale.joinToString { it.name }}")
                    }
                }
            }
    }

    private fun label(name: String, kind: NodeKind): ByteArray = buildJsonObject {
        put("name", name)
        put("kind", kind.name.lowercase())
    }.toString().encodeToByteArray().also {
        require(it.size <= 256) { "The device name and kind must fit a 256-byte label" }
    }

    private fun kind(device: Device): NodeKind = when (device.profile) {
        GrantProfile.Issuer -> NodeKind.Trusted
        NodeKind.Full.profile -> NodeKind.Full
        else -> NodeKind.Partial
    }

    private fun deviceLabel(bytes: ByteArray): DeviceRow? = runCatching {
        val label = Json.parseToJsonElement(bytes.decodeToString()).jsonObject
        DeviceRow("", label.getValue("name").jsonPrimitive.content, label.getValue("kind").jsonPrimitive.content)
    }.getOrNull()

    private suspend fun <T> attempt(what: String, block: suspend () -> T): T? = try {
        block()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Exception) {
        notes.tryEmit("Could not $what: ${error.message}")
        null
    }
}
