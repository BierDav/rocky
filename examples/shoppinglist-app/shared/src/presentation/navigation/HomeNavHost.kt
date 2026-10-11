package org.example.examples.shopping.presentation.navigation

import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberDecoratedNavEntries
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.scene.SceneDecoratorStrategy
import androidx.navigation3.ui.NavDisplay
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsKey
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsScreen
import org.example.examples.shopping.presentation.screen.devices.DevicesKey
import org.example.examples.shopping.presentation.screen.devices.DevicesScreen
import org.example.examples.shopping.presentation.screen.lists.ListsKey
import org.example.examples.shopping.presentation.screen.lists.ListsScreen
import org.example.examples.shopping.presentation.screen.sync.SyncKey
import org.example.examples.shopping.presentation.screen.sync.SyncScreen

/**
 * Home's tabs, each with its own back stack. A tab keeps its screens, saved state and ViewModels while another tab
 * shows; [tabs]' first is the start tab, which every other tab returns to on Back.
 */
@Stable
class HomeNavigation internal constructor(val tabs: List<NavKey>, private val stacks: List<NavBackStack<NavKey>>, selected: MutableIntState) {
    private var selected by selected

    /** The tab on screen. */
    var tab: NavKey
        get() = tabs[selected]
        set(value) {
            selected = tabs.indexOf(value).coerceAtLeast(0)
        }

    internal fun stack(tab: NavKey): NavBackStack<NavKey> = stacks[tabs.indexOf(tab)]

    /** Pops the current tab's stack, or returns to the start tab from another tab's root. */
    fun back() {
        val stack = stack(tab)
        if (stack.size > 1) stack.removeAt(stack.lastIndex) else tab = tabs.first()
    }
}

@Composable
fun rememberHomeNavigation(tabs: List<NavKey>): HomeNavigation {
    val stacks = tabs.map { rememberNavBackStack(navigationSavedStateConfig, it) }
    val selected = rememberSaveable { mutableIntStateOf(0) }
    return remember(tabs, stacks, selected) { HomeNavigation(tabs, stacks, selected) }
}

private val tab = NavigationSuiteSceneDecoratorStrategy.navigationSuite()

private val homeEntries = entryProvider<NavKey> {
    entry<ListsKey>(metadata = tab) { ListsScreen() }
    entry<ApprovalsKey>(metadata = tab) { ApprovalsScreen() }
    entry<DevicesKey>(metadata = tab) { DevicesScreen() }
    entry<SyncKey>(metadata = tab) { SyncScreen() }
}

/** Shows the current tab above the start tab, so Back reveals the start tab; [chrome] adds the bar or rail. */
@Composable
fun HomeNavHost(navigation: HomeNavigation, chrome: SceneDecoratorStrategy<NavKey>) {
    // One decorated entry list per tab keeps every tab's entries, and so their ViewModels, alive while hidden.
    val entries = navigation.tabs.map { tab ->
        rememberDecoratedNavEntries(
            navigation.stack(tab),
            listOf(rememberSaveableStateHolderNavEntryDecorator(), rememberViewModelStoreNavEntryDecorator()),
            homeEntries,
        )
    }
    val selected = navigation.tabs.indexOf(navigation.tab)
    NavDisplay(
        entries = if (selected == 0) entries.first() else entries.first() + entries[selected],
        sceneDecoratorStrategies = listOf(chrome),
        onBack = navigation::back,
    )
}
