package org.example.examples.shopping.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Gavel
import androidx.compose.material.icons.filled.Sync
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.merge
import rocky.ConnectionState
import rocky.ReadinessState

private enum class Tab(val label: String, val icon: ImageVector) {
    Lists("Lists", Icons.AutoMirrored.Filled.List),
    Approvals("Approvals", Icons.Filled.Gavel),
    Devices("Devices", Icons.Filled.Devices),
    Sync("Sync", Icons.Filled.Sync),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MainScreen(
    lists: ListsViewModel = metroViewModel(),
    approvals: ApprovalsViewModel = metroViewModel(),
    devices: DevicesViewModel = metroViewModel(),
    sync: SyncViewModel = metroViewModel(),
) {
    var tab by remember { mutableStateOf(Tab.Lists) }
    val snackbar = remember { SnackbarHostState() }
    val syncState by sync.state.collectAsState()
    LaunchedEffect(Unit) { merge(lists.messages, approvals.messages, devices.messages, sync.messages).collect { snackbar.showSnackbar(it) } }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (tab == Tab.Lists) "Shopping" else tab.label) },
                actions = {
                    SyncBadge(syncState, onClick = { tab = Tab.Sync })
                    Spacer(Modifier.width(8.dp))
                },
            )
        },
        bottomBar = {
            NavigationBar {
                Tab.entries.forEach { entry ->
                    NavigationBarItem(tab == entry, { tab = entry }, icon = { Icon(entry.icon, null) }, label = { Text(entry.label) })
                }
            }
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.padding(padding).fillMaxSize()) {
            when (tab) {
                Tab.Lists -> ListsScreen(lists)
                Tab.Approvals -> ApprovalsScreen(approvals)
                Tab.Devices -> DevicesScreen(devices)
                Tab.Sync -> SyncScreen(sync)
            }
        }
    }
}

/**
 * The sync state at a glance: green when the last pass reached a peer, amber while a scope bootstraps, grey offline
 * and red when no peer answered. It pulses while frames move.
 */
@Composable
fun SyncBadge(state: SyncState, onClick: () -> Unit) {
    val connection = state.connection
    val readiness = state.readiness?.state
    var now by remember { mutableStateOf(0L) }
    LaunchedEffect(Unit) {
        while (true) {
            now = currentMillis()
            delay(500)
        }
    }
    val moving = state.lastTransferAtEpochMillis?.let { now - it < 2_000 } == true || readiness == ReadinessState.Bootstrapping
    val (color, label) = when {
        readiness == ReadinessState.Bootstrapping -> Color(0xFFF9A825) to "Bootstrapping"
        connection.state == ConnectionState.Offline -> Color(0xFF9E9E9E) to "Offline"
        connection.state == ConnectionState.Connected && connection.peers > 0 -> Color(0xFF43A047) to (if (moving) "Syncing" else "Synced")
        state.serving -> Color(0xFF43A047) to "Serving devices"
        connection.state == ConnectionState.Connected -> Color(0xFF9E9E9E) to "No peers"
        connection.atEpochMillis == null -> Color(0xFF9E9E9E) to "Starting"
        else -> Color(0xFFE53935) to "Unreachable"
    }
    val animated by animateColorAsState(color)
    val pulse by rememberInfiniteTransition().animateFloat(0.35f, 1f, infiniteRepeatable(tween(600), RepeatMode.Reverse))
    AssistChip(
        onClick = onClick,
        label = { Text(if (connection.peers > 1 && label == "Synced") "$label · ${connection.peers} peers" else label) },
        leadingIcon = { Box(Modifier.size(10.dp).alpha(if (moving) pulse else 1f).background(animated, CircleShape)) },
    )
}

internal fun currentMillis(): Long = System.currentTimeMillis()

/** How long ago [epochMillis] was, coarsely. */
internal fun ago(epochMillis: Long?, now: Long = currentMillis()): String {
    if (epochMillis == null) return "never"
    val seconds = (now - epochMillis) / 1000
    return when {
        seconds < 2 -> "just now"
        seconds < 60 -> "${seconds}s ago"
        seconds < 3600 -> "${seconds / 60}m ago"
        seconds < 86_400 -> "${seconds / 3600}h ago"
        else -> "${seconds / 86_400}d ago"
    }
}
