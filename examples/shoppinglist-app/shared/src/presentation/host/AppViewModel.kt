package org.example.examples.shopping.presentation.host

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.withTimeoutOrNull
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.navigation.DeepLinks
import org.example.examples.shopping.util.splash.SplashController
import kotlin.time.Duration.Companion.seconds

/** The installation as the whole app sees it: the session's state, which picks the root screen, opened deep links and the launch splash. */
@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
class AppViewModel(private val session: ShoppingSession, deepLinks: DeepLinks, private val splash: SplashController) : ViewModel() {
    val state: StateFlow<SessionState> = session.state
    val deepLinks: Flow<NavKey> = deepLinks.destinations
    val splashVisible: StateFlow<Boolean> = splash.visible

    /** True once the session settled, or after [SPLASH_LIMIT]: an existing household opens straight into its lists. */
    val launched: StateFlow<Boolean> = flow {
        withTimeoutOrNull(SPLASH_LIMIT) { state.first { it != SessionState.Opening } }
        emit(true)
    }.stateIn(viewModelScope, SharingStarted.Eagerly, false)

    fun confirmRollout(digest: String) = session.confirmRollout(digest)

    fun hideSplash() = splash.hide()

    private companion object {
        val SPLASH_LIMIT = 2.seconds
    }
}
