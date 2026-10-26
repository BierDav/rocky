package org.example.examples.shopping.data.household

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import org.example.examples.shopping.data.db.DeviceRow
import org.example.examples.shopping.data.db.InvitationRow
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.TrustedRows
import org.example.examples.shopping.data.db.list
import rocky.Device
import rocky.GrantProfile
import rocky.PartitionRecipients
import rocky.Right
import rocky.RockyDatabase
import rocky.observeDevices
import rocky.observeInvitation
import rocky.room.settledOnRocky
import rocky.shareFollowing

/**
 * The household's devices and how list keys reach them, without a server: a trusted device records each device it
 * enrolls in `devices` and shares every list with trusted and full devices. Sync carries each envelope to its device
 * alone, directly or through the custody peer, and that device installs it; no row records who holds which list.
 * Partial devices receive only lists shared with them by [share], and the new epochs of a revocation's rotation.
 */
class Household(
    private val database: ShoppingDatabase,
    private val rocky: RockyDatabase,
    val self: String,

) {
    private val notes = MutableSharedFlow<String>(extraBufferCapacity = 64)

    /** The trusted-only tables; a member never touches them. */
    internal val trusted = TrustedRows(rocky)

    private val invitationsLock = Mutex()

    fun registry(): Flow<List<Device>> {
        check(rocky.isIssuer) { "Only trusted devices read the registry" }
        return rocky.credentials.observeDevices()
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
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    fun sent(): Flow<Map<String, Set<Long>>> = database.lists().observeAll().settledOnRocky()
        .flatMapLatest { lists ->
            if (lists.isEmpty()) return@flatMapLatest flowOf(emptyMap())
            rocky.partitions.observeStatuses(lists.map { rocky.list(it.id) }).map { states ->
                val sent = mutableMapOf<String, MutableSet<Long>>()
                for ((list, status) in lists.zip(states)) {
                    for (recipient in status.recipients.filter { it.epoch == status.current }) {
                        sent.getOrPut(recipient.node) { mutableSetOf() } += list.id
                    }
                }
                sent
            }
        }
        .distinctUntilChanged()

    /** What key delivery did, for the activity log. */
    val deliveries: SharedFlow<String> = notes.asSharedFlow()

    /** On a trusted device: a single-use code for one new device of [kind], replacing the previous one. */
    suspend fun invite(kind: NodeKind, name: String): PairingCode = invitationsLock.withLock {
        val invitation = rocky.credentials.pairing(kind.profile, label(name, kind))
        val code = PairingCode(kind, name, invitation.uri)
        // Other trusted devices list it too; members cannot read `invitations`.
        attempt("list the invitation") {
            trusted.openInvitation(self, kind.name.lowercase(), name, code.toString(), invitation.expiresAtEpochSeconds)
        }
        code
    }

    /** On a trusted device: each trusted device's open invitation, expired ones included. */
    fun invitations(): Flow<List<InvitationRow>> = if (!rocky.isIssuer) flowOf(emptyList()) else trusted.invitations().settledOnRocky()
    suspend fun record(node: String, name: String, kind: NodeKind) {
        check(rocky.isIssuer)
        val label = label(name, kind)
        if (node != self) rocky.setDeviceLabel(node, label)
        trusted.record(node, name, kind.name.lowercase())
    }

    /** Seals the held list to [node], chosen from the registry or discovered peers. */
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
            launch { closeUsedInvitations() }
            launch { deliverEveryList() }
        } else {
            val profile = rocky.session().profile as? GrantProfile.Grants
            if (profile?.grants?.any { it.table == "lists" && Right.Insert in it.rights } == true) deliverToIssuers()
        }
    }

    /** A full member sends new list keys to its known issuers, which distribute them to the household. */
    private suspend fun deliverToIssuers() = rocky.partitions.shareFollowing(
        combine(
            database.lists().observeAll().settledOnRocky(),
            rocky.observeStatus().map { status -> status.network.peers.filter { it.isIssuer }.map { it.node }.toSet() }.distinctUntilChanged(),
        ) { lists, issuers -> lists.map { PartitionRecipients(rocky.list(it.id), issuers) } },
        onFailure = { notes.tryEmit("Could not share lists: ${it.message}") },
    )

    private suspend fun recordEnrollments() {
        registry().collect { devices ->
            val known = trusted.devices().settledOnRocky().first().map { it.node }.toSet()
            for (device in devices.filter { !it.revoked && it.node !in known }) {
                val description = device.label?.let(::deviceLabel)
                val kind = description?.kind?.let(NodeKind::of) ?: kind(device)
                val name = description?.name ?: "Device ${device.node.take(6)}"
                attempt("record $name") { trusted.record(device.node, name, kind.name.lowercase()) }
            }
        }
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private suspend fun closeUsedInvitations() {
        trusted.invitations().settledOnRocky().flatMapLatest { rows ->
            val row = rows.firstOrNull { it.issuer == self } ?: return@flatMapLatest emptyFlow()
            rocky.credentials.observeInvitation(PairingCode.parse(row.code).invitation)
                .filter { !it.open }.take(1).onEach { trusted.closeInvitation(self, row.code) }
        }.collect()
    }

    /**
     * Shares each list's current epoch with every trusted and full device this device has not sent it to. A
     * revocation's rotation seals the new epoch to every device whose record reads the list, partial ones included.
     */
    private suspend fun deliverEveryList() = rocky.partitions.shareFollowing(
        combine(database.lists().observeAll().settledOnRocky(), devices(), revokedDevices()) { lists, devices, revoked ->
            val recipients = devices.filter { it.node != self && it.node !in revoked && NodeKind.of(it.kind)?.receivesEveryList == true }.map { it.node }.toSet()
            lists.map { PartitionRecipients(rocky.list(it.id), recipients) }
        },
        onFailure = { notes.tryEmit("Could not share lists: ${it.message}") },
    )

    private fun label(name: String, kind: NodeKind): ByteArray = buildJsonObject {
        put("name", name)
        put("kind", kind.name.lowercase())
    }.toString().encodeToByteArray().also {
        require(it.size <= 256) { "The device name and kind must fit a 256-byte label" }
    }

    // Only an application description enables every-list delivery.
    private fun kind(device: Device): NodeKind = if (device.profile == GrantProfile.Issuer) NodeKind.Trusted else NodeKind.Partial

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
