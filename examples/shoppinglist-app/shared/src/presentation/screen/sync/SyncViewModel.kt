package org.example.examples.shopping.presentation.screen.sync

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.screen.NodeViewModel
import rocky.*
import rocky.room.settledOnRocky

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class SyncViewModel(session: ShoppingSession) : NodeViewModel(session) {
    val filters = ActivityFilters()
    val entries: StateFlow<List<ActivityEntry>> = node.flatMapLatest { node ->
        val rocky = node.rocky
        val events = rocky.activity().shareIn(viewModelScope, SharingStarted.WhileSubscribed(), replay = KEPT)
        // Partition identities do not change with key epochs; resolve them when the list set changes.
        val lists = node.repository.lists().map { lists ->
            val partitions = lists.flatMap { list ->
                rocky.list(list.id).status().tables.map { it.partition to list.title }
            }.toMap()
            partitions to lists.associate { it.id to it.title }
        }.settledOnRocky()
        val log = merge(events.map { Entry.Event(it) }, node.household.deliveries.map { Entry.Note(it, kotlin.time.Clock.System.now().toEpochMilliseconds()) })
            .runningFold(emptyList<Entry>()) { kept, entry -> (listOf(entry) + kept).take(KEPT) }
        combine(log, if (node.isTrusted) node.household.devices() else flowOf(emptyList()), lists) { entries, devices, (partitions, titles) ->
            val names = devices.associate { it.node to it.name }
            entries.map { entry ->
                when (entry) {
                    is Entry.Event -> describe(entry.event, partitions, titles, names)

                    is Entry.Note -> ActivityEntry(
                        "note-${entry.at}-${entry.text.hashCode()}",
                        entry.at,
                        if (entry.text.startsWith("Could not")) EntryKind.Problem else EntryKind.Key,
                        entry.text,
                    )
                }
            }
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    private sealed interface Entry {
        data class Event(val event: RockyEvent) : Entry
        data class Note(val text: String, val at: Long) : Entry
    }
    private companion object {
        const val KEPT = 300
    }
}
