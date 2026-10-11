package org.example.examples.shopping.presentation.screen.home

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Gavel
import androidx.compose.material.icons.filled.Sync
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.presentation.component.LocalSnackbarHostState
import org.example.examples.shopping.presentation.component.SyncBadge
import org.example.examples.shopping.presentation.navigation.HomeNavHost
import org.example.examples.shopping.presentation.navigation.NavigationItem
import org.example.examples.shopping.presentation.navigation.NavigationSuiteSceneDecoratorStrategy
import org.example.examples.shopping.presentation.navigation.rememberHomeNavigation
import org.example.examples.shopping.presentation.screen.approvals.ApprovalsKey
import org.example.examples.shopping.presentation.screen.devices.DevicesKey
import org.example.examples.shopping.presentation.screen.lists.ListsKey
import org.example.examples.shopping.presentation.screen.sync.SyncKey

@Serializable
data object HomeKey : NavKey

/** Home's tabs; the first is the start tab. */
private val tabs = listOf(
    NavigationItem(ListsKey, "Lists", Icons.AutoMirrored.Filled.List),
    NavigationItem(ApprovalsKey, "Approvals", Icons.Filled.Gavel),
    NavigationItem(DevicesKey, "Devices", Icons.Filled.Devices),
    NavigationItem(SyncKey, "Sync", Icons.Filled.Sync),
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(model: HomeViewModel = metroViewModel()) {
    val navigation = rememberHomeNavigation(remember { tabs.map { it.key } })
    val snackbar = remember { SnackbarHostState() }
    val chrome = remember(navigation, model, snackbar) {
        NavigationSuiteSceneDecoratorStrategy<NavKey>(
            tabs,
            selected = { navigation.tab },
            onSelect = { navigation.tab = it },
            topBar = {
                val badge by model.badge.collectAsState()
                TopAppBar(
                    title = { Text(if (navigation.tab == ListsKey) "Shopping" else tabs.first { it.key == navigation.tab }.label) },
                    actions = {
                        SyncBadge(badge, onClick = { navigation.tab = SyncKey })
                        Spacer(Modifier.width(8.dp))
                    },
                )
            },
            snackbarHost = { SnackbarHost(snackbar) },
        )
    }
    CompositionLocalProvider(LocalSnackbarHostState provides snackbar) { HomeNavHost(navigation, chrome) }
}
