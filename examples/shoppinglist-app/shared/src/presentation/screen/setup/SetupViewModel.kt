package org.example.examples.shopping.presentation.screen.setup

import androidx.lifecycle.ViewModel
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import org.example.examples.shopping.data.session.ShoppingSession

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class)
class SetupViewModel(private val session: ShoppingSession) : ViewModel() {
    val state = session.state
    fun create() = session.create()
    fun join(code: String) = session.join(code)
    fun startOver() = session.startOver()
    fun retry() = session.retry()
    fun rejoin(code: String) = session.rejoin(code)
    fun reset() = session.reset()
}
