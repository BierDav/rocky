package org.example.examples.shopping.presentation.component

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.screen.NodeViewModel
import rocky.internal.native.DatabaseStatus

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class SyncBadgeViewModel(session: ShoppingSession) : NodeViewModel(session) {
    /** The sync badge in each tab's top bar. */
    val badge: StateFlow<DatabaseStatus?> = node.flatMapLatest { node ->
        node.rocky.observeStatus()
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), null)
}
