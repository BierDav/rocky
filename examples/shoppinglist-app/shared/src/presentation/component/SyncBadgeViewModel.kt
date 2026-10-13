package org.example.examples.shopping.presentation.component

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.screen.NodeViewModel
import rocky.LinkState
import rocky.RockyEvent

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class SyncBadgeViewModel(session: ShoppingSession) : NodeViewModel(session) {
    /** The sync badge in each tab's top bar. */
    val badge: StateFlow<SyncBadgeState> = node.flatMapLatest { node ->
        val rocky = node.rocky
        val lastTransfer = rocky.activity().filterIsInstance<RockyEvent.Synced>().map<RockyEvent.Synced, Long?> { it.atEpochMillis }.onStart { emit(null) }
        combine(rocky.status, rocky.observeReadiness(), lastTransfer) { connection, readiness, transfer ->
            SyncBadgeState(connection, readiness, transfer)
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SyncBadgeState())
}
