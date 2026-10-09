package org.example.examples.shopping.ui

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
import org.example.examples.shopping.*
import rocky.*

/** One held list and how this device holds each of its tables. */
data class ListHolding(val list: ShoppingList, val status: GroupStatus?)

/** Everything the device does in the background, for the sync screen and the top bar. */
data class SyncState(
    val readiness: Readiness? = null,
    val connection: ConnectionStatus = ConnectionStatus(ConnectionState.Unavailable),
    /** When frames last moved, for a pulse in the top bar. */
    val lastTransferAtEpochMillis: Long? = null,
    val self: SessionStatus? = null,
    val kind: NodeKind? = null,
    val domain: String = "",
    /** Other devices synced with this one recently, as on a trusted device that only serves. */
    val serving: Boolean = false,
    val holdings: List<ListHolding> = emptyList(),
    val entries: List<ActivityEntry> = emptyList(),
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class SyncViewModel(session: ShoppingSession) : NodeViewModel(session) {
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
        val log = merge(events.map { Entry.Event(it) }, node.household.deliveries.map { Entry.Note(it, System.currentTimeMillis()) })
            .runningFold(emptyList<Entry>()) { kept, entry -> (listOf(entry) + kept).take(KEPT) }
        val identity = flow { emit(rocky.session()) }
        // Served sessions grew since the last look: some device synced with this one.
        val serving = flow {
            var last = -1L
            while (true) {
                val network = rocky.network()
                emit(network.inboundActive > 0 || (last >= 0 && network.inboundServed > last))
                last = network.inboundServed
                delay(3_000)
            }
        }.distinctUntilChanged()
        combine(rocky.observeReadiness(), combine(rocky.status, serving, ::Pair), holdings, combine(log, node.household.devices(), identity, ::Triple)) { readiness, (connection, served), held, (entries, devices, self) ->
            val lists = held.flatMap { holding -> holding.status?.tables.orEmpty().map { it.partition to holding.list.title } }.toMap()
            val names = devices.associate { it.node to it.name }
            val titles = held.associate { it.list.id to it.list.title }
            SyncState(
                readiness, connection,
                entries.filterIsInstance<Entry.Event>().firstOrNull { it.event is RockyEvent.Synced }?.event?.atEpochMillis,
                self, devices.firstOrNull { it.node == self.node }?.kind?.let(NodeKind::of) ?: if (node.isTrusted) NodeKind.Trusted else null,
                shortDomain(rocky.domain), served, held,
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
            )
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SyncState())

    fun setOnline(online: Boolean) = write { rocky.setOnline(online) }
    fun syncNow() = write { rocky.syncOnce() }

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
