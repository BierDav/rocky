package org.example.examples.shopping.presentation.screen.pairing

import androidx.lifecycle.ViewModel
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.flow.StateFlow
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingSession

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
class PairingOfferViewModel(private val session: ShoppingSession) : ViewModel() {
    val state: StateFlow<SessionState> = session.state
    fun accept(code: PairingCode) = session.accept(code)
}
