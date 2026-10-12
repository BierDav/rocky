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
import rocky.ConnectionState
import rocky.ConnectionStatus
import rocky.ReadinessState

/** What the sync badge needs: the connection, bootstrapping, the last transfer and whether other devices sync here. */
data class SyncBadgeState(
    val connection: ConnectionStatus = ConnectionStatus(ConnectionState.Unavailable),
    val readiness: ReadinessState? = null,
    val lastTransferAtEpochMillis: Long? = null,
    val serving: Boolean = false,
    val live: Boolean = false,
)

/**
 * Shows Live while any link is open, then bootstrap, offline or last-pass status. It pulses while frames move.
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
    val moving = state.lastTransferAtEpochMillis?.let { now - it < 2_000 } == true || readiness == ReadinessState.Bootstrapping
    val (color, label) = when {
        state.live -> Color(0xFF43A047) to "Live"
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
