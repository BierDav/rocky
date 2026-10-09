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
import org.example.examples.shopping.*

data class ApprovalsState(
    val approvals: List<Approval> = emptyList(),
    val status: Coordinated = Coordinated.Unavailable,
    val trusted: Boolean = false,
    /** Requests this device is deciding, and asks it is sending, until their tickets settle. */
    val deciding: Set<String> = emptySet(),
    val asking: Int = 0,
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class ApprovalsViewModel(session: ShoppingSession) : NodeViewModel(session) {
    private val deciding = MutableStateFlow<Set<String>>(emptySet())
    private val asking = MutableStateFlow(0)

    val state: StateFlow<ApprovalsState> = node.flatMapLatest { node ->
        combine(node.approvals.requests(), node.approvals.status, deciding, asking) { approvals, status, deciding, asking ->
            ApprovalsState(approvals.reversed(), status, node.isTrusted, deciding, asking)
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), ApprovalsState())

    fun ask(title: String, note: String) {
        asking.update { it + 1 }
        write {
            try {
                approvals.ask(title, note)?.let { tell(it) }
            } finally {
                asking.update { it - 1 }
            }
        }
    }
    fun decide(key: String, approve: Boolean) {
        if (key in deciding.value) return
        deciding.update { it + key }
        write {
            try {
                approvals.decide(key, approve)?.let { tell(it) }
            } finally {
                deciding.update { it - key }
            }
        }
    }
}
