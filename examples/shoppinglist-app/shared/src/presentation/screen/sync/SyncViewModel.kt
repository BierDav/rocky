package org.example.examples.shopping.presentation.screen.sync

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.db.entity.ShoppingList
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.component.SyncBadgeState
import org.example.examples.shopping.presentation.screen.NodeViewModel
import rocky.*

/** One held list and how this device holds each of its tables. */
data class ListHolding(val list: ShoppingList, val status: GroupStatus?)

/** Everything the device does in the background, for the sync screen. */
data class SyncState(
    val readiness: Readiness? = null,
    val connection: ConnectionStatus = ConnectionStatus(ConnectionState.Unavailable),
    /** When frames last moved, for a pulse in the top bar. */
    val lastTransferAtEpochMillis: Long? = null,
    val self: SessionStatus? = null,
    val kind: NodeKind? = null,
    val domain: String = "",
    val holdings: List<ListHolding> = emptyList(),
    val entries: List<ActivityEntry> = emptyList(),
    val peers: List<Peer> = emptyList(),
) {
    val badge: SyncBadgeState get() = SyncBadgeState(connection, readiness?.state, lastTransferAtEpochMillis, peers.any { it.link.state == LinkState.Open })
}

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class SyncViewModel(private val session: ShoppingSession) : NodeViewModel(session) {
    val state: StateFlow<SyncState> = node.flatMapLatest { node ->
        val rocky = node.rocky
        val events = rocky.activity().shareIn(viewModelScope, SharingStarted.WhileSubscribed(), replay = KEPT)
        val ticks = flow {
            while (true) {
                emit(Unit)
                delay(3_000)
            }
        }
        // Holding changes with keys, checkpoints and evictions: read it again on each event and every few seconds.
        val holdings = combine(node.repository.lists(), merge(ticks, events.map { })) { lists, _ -> lists }
            .conflate()
            .map { lists -> lists.map { ListHolding(it, runCatching { rocky.list(it.id).status() }.getOrNull()) } }
        val log = merge(events.map { Entry.Event(it) }, node.household.deliveries.map { Entry.Note(it, kotlin.time.Clock.System.now().toEpochMilliseconds()) })
            .runningFold(emptyList<Entry>()) { kept, entry -> (listOf(entry) + kept).take(KEPT) }
        // Readiness does not emit for path measurements: read them again with the identity on each tick.
        val identity = ticks.map { rocky.session() to runCatching { rocky.network().peers }.getOrDefault(emptyList()) }
        combine(rocky.observeReadiness(), rocky.status, holdings, combine(log, if (node.isTrusted) node.household.devices() else flowOf(emptyList()), identity, ::Triple)) { readiness, connection, held, (entries, devices, identified) ->
            val (self, measured) = identified
            val lists = held.flatMap { holding -> holding.status?.tables.orEmpty().map { it.partition to holding.list.title } }.toMap()
            val paths = measured.associate { it.node to it.link }
            val peers = readiness.peers.map { peer ->
                paths[peer.node]?.takeIf { it.state == peer.link.state && it.openedAtEpochSeconds == peer.link.openedAtEpochSeconds }
                    ?.let { peer.copy(link = it) } ?: peer
            }
            val names = peers.associate { it.node to node.contacts.label(it.node) } + devices.associate { it.node to it.name }
            val titles = held.associate { it.list.id to it.list.title }
            SyncState(
                readiness, connection,
                entries.filterIsInstance<Entry.Event>().firstOrNull { it.event is RockyEvent.Synced }?.event?.atEpochMillis,
                self, node.kind,
                shortDomain(rocky.domain), held,
                entries.map { entry ->
                    when (entry) {
                        is Entry.Event -> describe(entry.event, lists, titles, names)

                        is Entry.Note -> ActivityEntry(
                            "note-${entry.at}-${entry.text.hashCode()}",
                            entry.at,
                            if (entry.text.startsWith("Could not")) EntryKind.Problem else EntryKind.Key,
                            entry.text,
                        )
                    }
                },
                peers,
            )
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SyncState())

    fun setOnline(online: Boolean) = write { rocky.setOnline(online) }
    fun syncNow() = write { rocky.syncOnce() }
    fun reset() = session.reset()

    private sealed interface Entry {
        data class Event(val event: RockyEvent) : Entry
        data class Note(val text: String, val at: Long) : Entry
    }
    private companion object {
        const val KEPT = 300
    }
}

/** The domain as a short hex id instead of its JSON byte arrays. */
private fun shortDomain(scope: String): String = Regex("\\d+").findAll(scope.substringAfter("\"domain\"")).take(8)
    .joinToString("") { it.value.toInt().toString(16).padStart(2, '0') }.ifEmpty { scope.take(16) }
