package org.example.examples.shopping.ui

import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import androidx.lifecycle.ViewModel
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.*
import rocky.NetworkStatus
import rocky.Peer
import rocky.SessionStatus

/** One device of the household as this device sees it: what a trusted device recorded and how sync reaches it. */
data class HouseholdDevice(
    val node: String,
    val name: String,
    val kind: NodeKind?,
    val isSelf: Boolean,
    /** Set when this device syncs with it directly. */
    val peer: Peer?,
    /** Titles of the lists whose key was sealed to it. */
    val lists: List<String>,
    val revoked: Boolean = false,
    val revocable: Boolean = false,
)

data class DevicesState(
    val self: SessionStatus? = null,
    val devices: List<HouseholdDevice> = emptyList(),
    val network: NetworkStatus? = null,
    val lists: List<ShoppingList> = emptyList(),
    val trusted: Boolean = false,
    /** The code a new device scans, while one is shown. */
    val invitation: PairingCode? = null,
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class DevicesViewModel(session: ShoppingSession) : NodeViewModel(session) {
    private val invitation = MutableStateFlow<PairingCode?>(null)
    val revoking = MutableStateFlow<Set<String>>(emptySet())

    val state: StateFlow<DevicesState> = node.flatMapLatest { node ->
        val reach = flow {
            while (true) {
                emit(Triple(node.rocky.session(), node.rocky.network(), if (node.isTrusted) node.rocky.devices() else emptyList()))
                delay(2_000)
            }
        }
        combine(node.household.devices(), node.household.keys(), node.repository.lists(), reach, invitation) { rows, keys, lists, (self, network, enrolled), code ->
            val titles = lists.associate { it.id to it.title }
            val recorded = rows.map { row ->
                HouseholdDevice(row.node, row.name, NodeKind.of(row.kind), row.node == self.node, network.peers.firstOrNull { it.node == row.node },
                    keys.filter { it.recipient == row.node }.mapNotNull { titles[it.list] }.distinct(),
                    revoked = enrolled.any { it.node == row.node && it.revoked },
                    revocable = row.node != self.node && enrolled.any { it.node == row.node && !it.revoked })
            }
            // Peers no trusted device recorded yet, such as an issuer that enrolled this device.
            val unrecorded = network.peers.filter { peer -> rows.none { it.node == peer.node } }.map { peer ->
                HouseholdDevice(peer.node, if (peer.isIssuer) "Trusted device" else "Device", if (peer.isIssuer) NodeKind.Trusted else null, false, peer, emptyList())
            }
            DevicesState(self, recorded + unrecorded, network, lists, node.isTrusted, code)
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DevicesState())

    /** Shows a code for one new device of [kind]; it replaces the previous code. */
    fun invite(kind: NodeKind, name: String) = write { invitation.value = household.invite(kind, name.ifBlank { "${kind.label} device" }) }
    fun dismissInvitation() { invitation.value = null }
    /** Sends a partial device the key of [list]. */
    fun share(list: Long, node: String) = write { household.share(list, node) }
    fun revoke(node: String) {
        if (node in revoking.value) return
        revoking.update { it + node }
        write {
            try { household.revoke(node) }
            finally { revoking.update { it - node } }
        }
    }
}
