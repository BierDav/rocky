package org.example.examples.shopping.presentation.component

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import org.example.examples.shopping.util.epochMillis
import rocky.BootstrapProgress
import rocky.ConnectionState
import rocky.ConnectionStatus
import rocky.ReadinessState

/** What the sync badge needs: the connection, bootstrapping and its progress, and the last transfer. */
data class SyncBadgeState(
    val connection: ConnectionStatus = ConnectionStatus(ConnectionState.Unavailable),
    val readiness: ReadinessState? = null,
    val lastTransferAtEpochMillis: Long? = null,
    val live: Boolean = false,
    val bootstrap: BootstrapProgress? = null,
)

/** "Bootstrapping 42%" once the offered rows are known; "Bootstrap paused 42%" while no peer link is open. */
fun bootstrapLabel(progress: BootstrapProgress?, paused: Boolean = false): String {
    val label = if (paused) "Bootstrap paused" else "Bootstrapping"
    return progress?.let { "$label ${it.percent}%" } ?: label
}

/**
 * Shows a paused bootstrap, Offline, then a bootstrap and its progress, then Live while any link is open, then the last
 * pass. It pulses while frames move.
 */
@Composable
fun SyncBadge(state: SyncBadgeState, onClick: () -> Unit) {
    val connection = state.connection
    val readiness = state.readiness
    var now by remember { mutableStateOf(0L) }
    LaunchedEffect(Unit) {
        while (true) {
            now = epochMillis()
            delay(500)
        }
    }
    val bootstrapping = readiness == ReadinessState.Bootstrapping
    // A bootstrap moves only over an open link; offline or unlinked, its imported pages wait.
    val paused = bootstrapping && (connection.state == ConnectionState.Offline || !state.live)
    val moving = state.lastTransferAtEpochMillis?.let { now - it < 2_000 } == true || (bootstrapping && !paused)
    val (color, label) = when {
        paused -> Color(0xFF9E9E9E) to bootstrapLabel(state.bootstrap, paused = true)
        connection.state == ConnectionState.Offline -> Color(0xFF9E9E9E) to "Offline"
        bootstrapping -> Color(0xFFF9A825) to bootstrapLabel(state.bootstrap)
        state.live -> Color(0xFF43A047) to "Live"
        connection.state == ConnectionState.Connected && connection.peers > 0 -> Color(0xFF43A047) to (if (moving) "Syncing" else "Synced")
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
