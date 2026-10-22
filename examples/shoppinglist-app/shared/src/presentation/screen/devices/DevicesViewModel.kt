package org.example.examples.shopping.presentation.screen.devices

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.withTimeoutOrNull
import org.example.examples.shopping.data.db.entity.ShoppingList
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.screen.NodeViewModel
import rocky.DatabaseStatus
import rocky.Device
import rocky.NetworkStatus
import rocky.Peer
import rocky.RockyEvent
import rocky.SessionStatus
import rocky.room.settledOnRocky

/** One device of the household as this device sees it: what a trusted device recorded and how sync reaches it. */
data class HouseholdDevice(
    val node: String,
    val name: String,
    val kind: NodeKind?,
    val isSelf: Boolean,
    /** Set when this device syncs with it directly. */
    val peer: Peer?,
    /** On a trusted device: titles of the lists whose current key this device sent it. */
    val lists: List<String>,
    val revoked: Boolean = false,
    val revocable: Boolean = false,
)

/** A trusted device's pairing code that is still open: unused and not expired. */
data class OpenInvitation(val code: PairingCode, val issuer: String, val mine: Boolean, val expiresAtEpochSeconds: Long)

data class DevicesState(
    val self: SessionStatus? = null,
    /** This device, whether or not a trusted device recorded it yet. */
    val me: HouseholdDevice? = null,
    /** The other devices that may still sync. */
    val devices: List<HouseholdDevice> = emptyList(),
    val revoked: List<HouseholdDevice> = emptyList(),
    val network: NetworkStatus? = null,
    val lists: List<ShoppingList> = emptyList(),
    val trusted: Boolean = false,
    /** The code a new device scans, while one is shown. */
    val invitation: PairingCode? = null,
    /** On a trusted device: every trusted device's open invitation, soonest to expire first. */
    val invitations: List<OpenInvitation> = emptyList(),
    val snapshot: DatabaseStatus? = null,
    val lastTransferAtEpochMillis: Long? = null,
    val domain: String = "",
    /** The persisted **Networking enabled** switch. */
    val online: Boolean = true,
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class DevicesViewModel(private val session: ShoppingSession) : NodeViewModel(session) {
    private val invitation = MutableStateFlow<PairingCode?>(null)
    val revoking = MutableStateFlow<Set<String>>(emptySet())
    private val refresh = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    val state: StateFlow<DevicesState> = node.flatMapLatest { node ->
        val reach = flow {
            while (true) {
                emit(
                    Reach(
                        node.rocky.session(),
                        node.rocky.network(),
                        if (node.isTrusted) node.rocky.devices() else emptyList(),
                    ),
                )
                withTimeoutOrNull(2_000) { refresh.first() }
            }
        }.settledOnRocky()
        val recorded = combine(if (node.isTrusted) node.household.devices() else flowOf(emptyList()), node.household.invitations(), ::Pair)
        val sent = if (node.isTrusted) node.household.sent() else flowOf(emptyMap())
        val devices = combine(recorded, sent, node.repository.lists(), reach, invitation) { (rows, opened), sent, lists, reach, code ->
            val (self, network, enrolled) = reach
            val titles = lists.associate { it.id to it.title }
            val recorded = rows.map { row ->
                HouseholdDevice(
                    row.node,
                    row.name,
                    NodeKind.of(row.kind),
                    row.node == self.node,
                    network.peers.firstOrNull { it.node == row.node },
                    sent[row.node].orEmpty().mapNotNull { titles[it] }.sorted(),
                    revoked = enrolled.any { it.node == row.node && it.revoked },
                    revocable = row.node != self.node && enrolled.any { it.node == row.node && !it.revoked },
                )
            }
            // Peers no trusted device recorded yet, such as an issuer that enrolled this device.
            val unrecorded = network.peers.filter { peer -> peer.node != self.node && rows.none { it.node == peer.node } }.map { peer ->
                HouseholdDevice(peer.node, "Device ${peer.node.take(6)}", null, false, peer, emptyList())
            }
            // Until a trusted device's record of it syncs, this device describes itself from its own grants.
            val me = recorded.firstOrNull { it.isSelf }
                ?: HouseholdDevice(self.node, "This device", node.kind, true, null, emptyList())
            val others = (recorded + unrecorded).filter { !it.isSelf }
            val now = kotlin.time.Clock.System.now().toEpochMilliseconds() / 1000
            val invitations = opened.filter { it.expires > now }.mapNotNull { row ->
                val pairing = runCatching { PairingCode.parse(row.code) }.getOrNull() ?: return@mapNotNull null
                OpenInvitation(pairing, rows.firstOrNull { it.node == row.issuer }?.name ?: "Device ${row.issuer.take(6)}", row.issuer == self.node, row.expires)
            }
            DevicesState(
                self, me, others.filter { !it.revoked }, others.filter { it.revoked }, network, lists, node.isTrusted, code,
                invitations,
            )
        }
        val lastTransfer = node.rocky.activity().filterIsInstance<RockyEvent.Synced>()
            .map<RockyEvent.Synced, Long?> { it.atEpochMillis }.onStart { emit(null) }
        combine(devices, node.rocky.observeStatus(), lastTransfer, node.online) { state, snapshot, transfer, online ->
            state.copy(snapshot = snapshot, lastTransferAtEpochMillis = transfer, domain = node.rocky.domain, online = online)
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DevicesState())

    private data class Reach(val self: SessionStatus, val network: NetworkStatus, val enrolled: List<Device>)

    /** Shows a code for one new device of [kind]; it replaces the previous code. */
    fun invite(kind: NodeKind, name: String) = write { invitation.value = household.invite(kind, name.ifBlank { "${kind.label} device" }) }
    fun dismissInvitation() {
        invitation.value = null
    }

    fun setNetworkingEnabled(online: Boolean) = write { setOnline(online) }
    fun reset() = session.reset()

    /** Sends a partial device the key of [list]. */
    fun share(list: Long, node: String) = write { household.share(list, node) }
    fun revoke(node: String) {
        if (node in revoking.value) return
        revoking.update { it + node }
        write {
            try {
                household.revoke(node)
            } finally {
                revoking.update { it - node }
            }
        }
    }

    /** Removes the application description; the registry tombstone remains authoritative. */
    fun forget(node: String) = write {
        household.forget(node)
        refresh.tryEmit(Unit)
    }
}
