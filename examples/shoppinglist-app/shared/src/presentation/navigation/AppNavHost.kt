package org.example.examples.shopping.presentation.navigation

import androidx.compose.runtime.Composable
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.scene.DialogSceneStrategy
import androidx.navigation3.scene.SinglePaneSceneStrategy
import androidx.navigation3.ui.NavDisplay
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.presentation.screen.home.HomeKey
import org.example.examples.shopping.presentation.screen.home.HomeScreen
import org.example.examples.shopping.presentation.screen.opening.OpeningKey
import org.example.examples.shopping.presentation.screen.opening.OpeningScreen
import org.example.examples.shopping.presentation.screen.pairing.PairingOfferKey
import org.example.examples.shopping.presentation.screen.pairing.PairingOfferScreen
import org.example.examples.shopping.presentation.screen.resetting.ResettingKey
import org.example.examples.shopping.presentation.screen.resetting.ResettingScreen
import org.example.examples.shopping.presentation.screen.revoked.RevokedKey
import org.example.examples.shopping.presentation.screen.revoked.RevokedScreen
import org.example.examples.shopping.presentation.screen.setup.SetupKey
import org.example.examples.shopping.presentation.screen.setup.SetupScreen

/** The root destination of each session state: setup until this device belongs to a household, then home. */
val SessionState.destination: NavKey
    get() = when (this) {
        SessionState.Opening -> OpeningKey
        SessionState.Resetting -> ResettingKey
        SessionState.Setup, is SessionState.Failed -> SetupKey
        is SessionState.Open -> HomeKey
        is SessionState.Revoked -> RevokedKey
    }

/** Shows [backStack]; each entry keeps its own saved state and ViewModels until it leaves the stack. Deep links open as dialogs. */
@Composable
fun AppNavHost(backStack: MutableList<NavKey>) {
    NavDisplay(
        backStack = backStack,
        sceneStrategies = listOf(DialogSceneStrategy(), SinglePaneSceneStrategy()),
        entryDecorators = listOf(
            rememberSaveableStateHolderNavEntryDecorator(),
            rememberViewModelStoreNavEntryDecorator(),
        ),
        entryProvider = entryProvider {
            entry<OpeningKey> { OpeningScreen() }
            entry<SetupKey> { SetupScreen() }
            entry<HomeKey> { HomeScreen() }
            entry<RevokedKey> { RevokedScreen() }
            entry<ResettingKey> { ResettingScreen() }
            entry<PairingOfferKey>(metadata = DialogSceneStrategy.dialog()) { key -> PairingOfferScreen(key, onDone = { backStack.remove(key) }) }
        },
    )
}
