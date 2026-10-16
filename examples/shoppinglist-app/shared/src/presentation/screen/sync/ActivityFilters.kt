package org.example.examples.shopping.presentation.screen.sync

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** Groups may overlap: a failed connection appears under both Problems and Connections. */
enum class ActivityFilter(val label: String) {
    Problems("Problems"),
    Transfers("Transfers"),
    Connections("Connections"),
    Keys("Keys"),
    Access("Access"),
    Devices("Devices"),
    History("History"),
    ;

    fun matches(entry: ActivityEntry): Boolean = when (this) {
        Problems -> entry.isProblem
        Transfers -> entry.kind == EntryKind.Transfer
        Connections -> entry.kind in setOf(EntryKind.PeerUp, EntryKind.PeerDown, EntryKind.Network)
        Keys -> entry.kind == EntryKind.Key
        Access -> entry.kind in setOf(EntryKind.Credentials, EntryKind.Author)
        Devices -> entry.kind == EntryKind.Device
        History -> entry.kind == EntryKind.Checkpoint
    }
}

/** Screen-owned editor state survives new events and tab changes. */
class ActivityFilters {
    var search by mutableStateOf("")
    var selected by mutableStateOf(emptySet<ActivityFilter>())
        private set

    fun toggle(filter: ActivityFilter) {
        selected = if (filter in selected) selected - filter else selected + filter
    }
    fun all() {
        selected = emptySet()
    }
    fun clear() {
        all()
        search = ""
    }
}

/** Search terms intersect; selected categories combine, preserving the newest-first input order. */
fun filterActivity(entries: List<ActivityEntry>, search: String, selected: Set<ActivityFilter> = emptySet()): List<ActivityEntry> {
    val terms = search.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
    return entries.filter { entry ->
        val text = "${entry.title}\n${entry.detail.orEmpty()}\n${entry.key}"
        terms.all { text.contains(it, ignoreCase = true) } && (selected.isEmpty() || selected.any { it.matches(entry) })
    }
}
