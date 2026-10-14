package org.example.examples.shopping.presentation.host

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner
import dev.zacsweers.metrox.viewmodel.LocalMetroViewModelFactory
import dev.zacsweers.metrox.viewmodel.metroViewModel
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.di.ShoppingGraph
import org.example.examples.shopping.presentation.navigation.AppNavHost
import org.example.examples.shopping.presentation.navigation.destination
import org.example.examples.shopping.presentation.navigation.rememberNavigator
import org.example.examples.shopping.presentation.navigation.tabs
import org.example.examples.shopping.presentation.theme.ShoppingTheme
import org.example.examples.shopping.util.permissions.NetworkPermissions
import rocky.RolloutConfirmationRequired

/** The whole app: setup until this device belongs to a household, then its lists, devices and sync. */
@Composable
fun ShoppingApp(graph: ShoppingGraph) {
    // Hosts without their own owner, such as the desktop window, keep the app's ViewModels here.
    val fallback = remember {
        object : ViewModelStoreOwner {
            override val viewModelStore = ViewModelStore()
        }
    }
    DisposableEffect(fallback) {
        onDispose { fallback.viewModelStore.clear() }
    }
    val owner = LocalViewModelStoreOwner.current ?: fallback
    CompositionLocalProvider(
        LocalMetroViewModelFactory provides graph.metroViewModelFactory,
        LocalViewModelStoreOwner provides owner,
    ) {
        ShoppingTheme {
            NetworkPermissions()
            Surface(Modifier.fillMaxSize()) {
                AppHost()
            }
        }
    }
}

@Composable
private fun AppHost(model: AppViewModel = metroViewModel()) {
    val state by model.state.collectAsState()
    val navigator = rememberNavigator(remember { tabs.map { it.key } }, state.destination)
    // The session decides the root: entering a household, being revoked or a reset replaces what shows.
    LaunchedEffect(navigator, state.destination) { navigator.showRoot(state.destination) }
    // A deep link opens above the current screen, replacing one opened before it.
    LaunchedEffect(navigator) { model.deepLinks.collect(navigator::navigate) }
    Box(Modifier.fillMaxSize()) {
        AppNavHost(navigator)
        LaunchScreen(model.splashVisible.collectAsState().value)
    }
    val required = (state as? SessionState.Failed)?.error as? RolloutConfirmationRequired
    var dismissed by remember(required) { mutableStateOf(false) }
    if (required != null && !dismissed) {
        AlertDialog(
            onDismissRequest = { dismissed = true },
            title = { Text("Remove data from this household?") },
            text = {
                Column(Modifier.heightIn(max = 400.dp).verticalScroll(rememberScrollState())) {
                    Text("This update removes the following data on every device that adopts it. Counts are for this device only.")
                    required.report.tables.forEach { Text("Table ${it.name}: ${it.rows} local rows") }
                    required.report.fields.forEach { Text("Field ${it.name}: ${it.rows} local rows") }
                    Text("Removal cannot be undone. Other devices may hold more data.")
                }
            },
            confirmButton = { TextButton(onClick = { model.confirmRollout(required.report.digest) }) { Text("Remove and update") } },
            dismissButton = { TextButton(onClick = { dismissed = true }) { Text("Cancel") } },
        )
    }
    // The splash lifts once the settled root screen has been composed and drawn.
    val launched by model.launched.collectAsState()
    LaunchedEffect(launched) {
        if (launched) {
            repeat(2) { withFrameNanos {} }
            model.hideSplash()
        }
    }
}
