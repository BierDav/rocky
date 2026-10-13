package org.example.examples.shopping.presentation.component

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import rocky.BootstrapProgress
import rocky.DatabaseStatus
import rocky.LinkState

fun bootstrapPercent(progress: BootstrapProgress): Int = if (progress.rowsTotal == 0uL) 100 else (progress.rowsDone.toDouble() * 100 / progress.rowsTotal.toDouble()).toInt()
fun bootstrapLabel(progress: BootstrapProgress?, paused: Boolean = false): String {
    val label = if (paused) "Bootstrap paused" else "Importing"
    return progress?.let { "$label ${bootstrapPercent(it)}%" } ?: label
}

@Composable
fun SyncBadge(state: DatabaseStatus?, onClick: (() -> Unit)? = null) {
    val open = state?.network?.peers?.count { it.link.state == LinkState.Open } ?: 0
    val (color, label) = when {
        state == null -> Color(0xFF9E9E9E) to "Observing status"
        !state.network.networkingEnabled -> Color(0xFF9E9E9E) to "Networking disabled"
        !state.network.foreground -> Color(0xFF9E9E9E) to "Suspended in background"
        state.data.bootstrapPending -> Color(0xFFF9A825) to bootstrapLabel(state.data.bootstrap)
        open > 0 -> Color(0xFF43A047) to "$open peer links"
        state.network.peers.any { it.link.state == LinkState.Opening } -> Color(0xFFF9A825) to "Opening peer links"
        else -> Color(0xFF9E9E9E) to "No peer links"
    }
    val animated by animateColorAsState(color)
    val content: @Composable () -> Unit = {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(Modifier.size(10.dp).background(animated, CircleShape))
            Text(label)
        }
    }
    if (onClick == null) {
        Surface(shape = MaterialTheme.shapes.small, color = MaterialTheme.colorScheme.surfaceContainerHigh) {
            Box(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) { content() }
        }
    } else {
        AssistChip(onClick = onClick, label = content)
    }
}
