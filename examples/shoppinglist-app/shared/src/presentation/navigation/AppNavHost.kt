package org.example.examples.shopping.presentation.navigation

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberDecoratedNavEntries
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.scene.DialogSceneStrategy
import androidx.navigation3.scene.SinglePaneSceneStrategy
import androidx.navigation3.ui.NavDisplay
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.presentation.component.TabTopBar
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsKey
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsScreen
import org.example.examples.shopping.presentation.screen.devices.DevicesKey
import org.example.examples.shopping.presentation.screen.devices.DevicesScreen
import org.example.examples.shopping.presentation.screen.history.HistoryKey
import org.example.examples.shopping.presentation.screen.history.HistoryScreen
import org.example.examples.shopping.presentation.screen.lists.ListsKey
import org.example.examples.shopping.presentation.screen.lists.ListsScreen
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
import org.example.examples.shopping.presentation.screen.sync.SyncKey
import org.example.examples.shopping.presentation.screen.sync.SyncScreen

/** The household's tabs; the first is the start tab. */
val tabs = listOf(
    NavigationItem(ListsKey, "Lists", Icons.AutoMirrored.Filled.List),
    NavigationItem(ApprovalsKey, "Approvals", Icons.Filled.Gavel),
    NavigationItem(DevicesKey, "Devices", Icons.Filled.Devices),
    NavigationItem(SyncKey, "Sync", Icons.Filled.Sync),
    NavigationItem(HistoryKey, "History", Icons.Filled.History),
)

/** The root screen of each session state; null while the household is open, when its tabs show. */
val SessionState.destination: NavKey?
    get() = when (this) {
        SessionState.Opening -> OpeningKey
        SessionState.Resetting -> ResettingKey
        SessionState.Setup, is SessionState.Failed -> SetupKey
        is SessionState.Open -> null
        is SessionState.Revoked -> RevokedKey
    }

/**
 * The app's one navigation host. Each stack's entries keep their saved state and ViewModels until they leave it;
 * tab screens show inside the navigation bar or rail, every screen gets the snackbar, and deep links open as dialogs.
 */
@Composable
fun AppNavHost(navigator: Navigator) {
    val tab = NavigationSuiteSceneDecoratorStrategy.navigationSuite()
    val provider = entryProvider {
        entry<OpeningKey> { OpeningScreen() }
        entry<SetupKey> { SetupScreen() }
        entry<RevokedKey> { RevokedScreen() }
        entry<ResettingKey> { ResettingScreen() }
        entry<ListsKey>(metadata = tab) { TabTopBar("Shopping") { ListsScreen() } }
        entry<ApprovalsKey>(metadata = tab) { TabTopBar("Approvals") { ApprovalsScreen() } }
        entry<DevicesKey>(metadata = tab) { TabTopBar("Devices") { DevicesScreen() } }
        entry<SyncKey>(metadata = tab) { TabTopBar("Sync") { SyncScreen() } }
        entry<HistoryKey>(metadata = tab) { TabTopBar("History") { HistoryScreen() } }
        entry<PairingOfferKey>(metadata = DialogSceneStrategy.dialog()) { key ->
            PairingOfferScreen(
                key,
                onDone = { navigator.remove(key) },
            )
        }
    }
    // One decorated entry list per stack keeps every tab's entries, and so their ViewModels, alive while hidden.
    val root = rememberDecoratedNavEntries(navigator.root, rememberEntryDecorators(), provider)
    val tabEntries =
        navigator.tabStacks.map { stack -> rememberDecoratedNavEntries(stack, rememberEntryDecorators(), provider) }
    val selected = navigator.tabs.indexOf(navigator.tab)
    val entries = when {
        !navigator.inTabs -> root

        selected == 0 -> tabEntries.first()

        // The start tab stays below another tab, so Back and predictive back reveal it.
        else -> tabEntries.first() + tabEntries[selected]
    }
    val snackbar = remember { SnackbarHostState() }
    val decorators = remember(navigator, snackbar) {
        listOf(
            SnackbarSceneDecoratorStrategy<NavKey>(snackbar),
            NavigationSuiteSceneDecoratorStrategy(tabs, navigator),
        )
    }
    CompositionLocalProvider(LocalNavigator provides navigator) {
        NavDisplay(
            entries = entries,
            sceneStrategies = listOf(DialogSceneStrategy(), SinglePaneSceneStrategy()),
            sceneDecoratorStrategies = decorators,
            onBack = navigator::back,
        )
    }
}

@Composable
private fun rememberEntryDecorators() = listOf(
    rememberSaveableStateHolderNavEntryDecorator<NavKey>(),
    rememberViewModelStoreNavEntryDecorator<NavKey>(),
)
