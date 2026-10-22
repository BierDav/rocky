package org.example.examples.shopping.presentation.screen.history

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
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.data.tips.Follow
import org.example.examples.shopping.data.tips.ListSnapshot
import org.example.examples.shopping.data.tips.TipsState
import org.example.examples.shopping.presentation.screen.NodeViewModel

data class HistoryState(
    val tips: TipsState? = null,
    val trusted: Boolean = false,
    /** Why the last seal policy round failed. */
    val problem: String? = null,
)

/** A seal's lists as a read-only view showed them. */
data class SealView(val seal: String, val lists: List<ListSnapshot>)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class HistoryViewModel(private val session: ShoppingSession) : NodeViewModel(session) {
    val view = MutableStateFlow<SealView?>(null)

    val state: StateFlow<HistoryState> = node.flatMapLatest { node ->
        val refreshed = flow {
            while (true) {
                emit(runCatching { node.tips.refresh() }.getOrNull())
                delay(2_000)
            }
        }.filterNotNull()
        combine(refreshed, node.tips.problem) { tips, problem -> HistoryState(tips, node.isTrusted, problem) }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), HistoryState())

    /**
     * Follows [name]. A trusted device following a bookmark whose schema is older than this app's seals it and
     * reopens, which publishes this app's schema there; with networking off it stays on this device until switched on.
     */
    fun follow(name: String) = write {
        var outcome = tips.follow(name)
        // Another schema replaces this one at the end of the next sync pass.
        while (outcome == Follow.Pending) {
            delay(500)
            outcome = tips.switched()
        }
        when (outcome) {
            Follow.Switched, Follow.Pending -> {}

            Follow.Pulling -> tell("Fetching $name's starting state from a device that keeps it; switching once it arrives")

            Follow.Ahead -> if (isTrusted) {
                session.reopen { it.tips.seal() }
            } else {
                tell("$name has an older schema; a trusted device publishes this app's schema there")
            }
        }
    }

    fun branch(name: String) = write { tips.branch(name.trim()) }
    fun leave(name: String, delete: Boolean) = write { tips.leave(name, delete) }
    fun seal() = write { tips.seal() }
    fun rollBack(seal: String) = write { tips.rollBack(seal) }
    fun reauthor(id: String) = write { tips.reauthor(id) }
    fun dropHeld(id: String) = write { tips.dropHeld(id) }
    fun show(seal: String) = write { view.value = SealView(seal, tips.snapshot(seal)) }
    fun closeView() {
        view.value = null
    }
}
