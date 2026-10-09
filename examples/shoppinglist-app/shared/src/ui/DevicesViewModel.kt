package org.example.examples.shopping.ui

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
import org.example.examples.shopping.*
import rocky.Device
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
    /** The latest horizon that purges removed devices' records, on a trusted device. */
    val purge: Purge? = null,
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class DevicesViewModel(session: ShoppingSession) : NodeViewModel(session) {
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
                        if (node.isTrusted) runCatching { node.household.purgeStatus() }.getOrNull() else null,
                    ),
                )
                withTimeoutOrNull(2_000) { refresh.first() }
            }
        }
        val recorded = combine(node.household.devices(), node.household.invitations(), ::Pair)
        combine(recorded, node.household.keys(), node.repository.lists(), reach, invitation) { (rows, opened), keys, lists, reach, code ->
            val (self, network, enrolled, purge) = reach
            val titles = lists.associate { it.id to it.title }
            val recorded = rows.map { row ->
                HouseholdDevice(
                    row.node,
                    row.name,
                    NodeKind.of(row.kind),
                    row.node == self.node,
                    network.peers.firstOrNull { it.node == row.node },
                    keys.filter { it.recipient == row.node }.mapNotNull { titles[it.list] }.distinct(),
                    revoked = enrolled.any { it.node == row.node && it.revoked },
                    revocable = row.node != self.node && enrolled.any { it.node == row.node && !it.revoked },
                )
            }
            // Peers no trusted device recorded yet, such as an issuer that enrolled this device.
            val unrecorded = network.peers.filter { peer -> rows.none { it.node == peer.node } }.map { peer ->
                HouseholdDevice(peer.node, if (peer.isIssuer) "Trusted device" else "Device", if (peer.isIssuer) NodeKind.Trusted else null, false, peer, emptyList())
            }
            // Until a trusted device's record of it syncs, this device describes itself from its own grants.
            val me = recorded.firstOrNull { it.isSelf }
                ?: HouseholdDevice(self.node, "This device", if (self.isIssuer) NodeKind.Trusted else null, true, null, emptyList())
            val others = (recorded + unrecorded).filter { !it.isSelf }
            val now = System.currentTimeMillis() / 1000
            val invitations = opened.filter { it.expires > now }.mapNotNull { row ->
                val pairing = runCatching { PairingCode.parse(row.code) }.getOrNull() ?: return@mapNotNull null
                OpenInvitation(pairing, rows.firstOrNull { it.node == row.issuer }?.name ?: "Device ${row.issuer.take(6)}", row.issuer == self.node, row.expires)
            }
            DevicesState(
                self, me, others.filter { !it.revoked }, others.filter { it.revoked }, network, lists, node.isTrusted, code,
                invitations, purge,
            )
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DevicesState())

    private data class Reach(val self: SessionStatus, val network: NetworkStatus, val enrolled: List<Device>, val purge: Purge?)

    /** Shows a code for one new device of [kind]; it replaces the previous code. */
    fun invite(kind: NodeKind, name: String) = write { invitation.value = household.invite(kind, name.ifBlank { "${kind.label} device" }) }
    fun dismissInvitation() {
        invitation.value = null
    }

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

    /** Removes a revoked device's record now and declares the horizon that purges its trace. */
    fun forget(node: String) = write {
        household.forget(node)
        try {
            household.purge()
        } catch (error: Exception) {
            // The record is gone either way; only the purge waits until this device met every other one.
            tell("Removed. Purging its trace needs a sync with every device first: ${error.message}")
        }
        refresh.tryEmit(Unit)
    }
}
