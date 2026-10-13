package org.example.examples.shopping.presentation.screen.devices

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.presentation.component.LinkPaths
import org.example.examples.shopping.util.ago
import org.example.examples.shopping.util.date
import rocky.AttemptOutcome
import rocky.LinkState
import rocky.Peer

@Composable
internal fun DeviceConnection(peer: Peer, now: Long) {
    val summary = connectionSummary(peer, now)
    val color = when (summary.state) {
        LinkState.Open -> Color(0xFF43A047)
        LinkState.Opening, LinkState.Closing -> Color(0xFFF9A825)
        LinkState.Closed -> if (peer.attempts.any { it.outcome == AttemptOutcome.Failed }) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Box(Modifier.size(8.dp).background(color, CircleShape))
        Text(summary.title, style = MaterialTheme.typography.bodyMedium)
    }
    Text(
        buildString {
            append(peer.source.replace('_', ' ').replaceFirstChar(Char::uppercase))
            if (peer.isLocal) append(" · local network")
            if (peer.isIssuer) append(" · issuer")
            append(" · ").append(selectionReason(peer.choice))
        },
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    summary.error?.let {
        SelectionContainer { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
    }
    LinkPaths(peer.link)
}

internal data class ConnectionSummary(val state: LinkState, val title: String, val error: String?)

internal fun connectionSummary(peer: Peer, now: Long): ConnectionSummary {
    val attempt = peer.attempts.maxWithOrNull(compareBy({ it.endedAtEpochMillis ?: it.startedAtEpochMillis }, { it.id }))
    val checked = ago(attempt?.endedAtEpochMillis?.toLong(), now)
    val title = when (peer.link.state) {
        LinkState.Open -> peer.link.openedAtEpochSeconds?.let { "Link open for ${connectionDuration(it.toLong(), now)}" } ?: "Link open"
        LinkState.Opening -> "Opening link"
        LinkState.Closing -> "Closing link"
        LinkState.Closed -> "Link closed" + (attempt?.let { " · last ${it.operation}: ${it.outcome ?: it.stage} $checked" } ?: "")
    }
    val error = attempt?.failure?.let { "${attempt.stage} failed: ${it.detail}" }
    return ConnectionSummary(peer.link.state, title, error)
}

/** Elapsed time of the current link; a replacement link supplies a fresh opening time. */
internal fun connectionDuration(openedAtEpochSeconds: Long, nowEpochMillis: Long): String {
    val seconds = (nowEpochMillis / 1_000 - openedAtEpochSeconds).coerceAtLeast(0)
    val days = seconds / 86_400
    val hours = seconds / 3_600 % 24
    val minutes = seconds / 60 % 60
    return buildString {
        if (days > 0) append("${days}d ")
        if (hours > 0 || days > 0) append("${hours}h ")
        if (minutes > 0 || hours > 0 || days > 0) append("${minutes}m ")
        append("${seconds % 60}s")
    }
}

private fun selectionReason(choice: String): String = when (choice) {
    "selected_local" -> "selected for proximity"
    "selected_holding" -> "selected for shared data"
    "selected_trusted" -> "selected for trusted access"
    "selected_probe" -> "selected to probe"
    "backoff" -> "retry backoff"
    "pass_limit" -> "waiting for a sync slot"
    "covered" -> "data covered by another peer"
    "not_considered" -> "not selected yet"
    else -> choice.replace('_', ' ')
}

@Composable
internal fun DeviceDetails(node: String, peer: Peer?) {
    var expanded by rememberSaveable(node) { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().clickable { expanded = !expanded }.padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Identity and routes", style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.onSurfaceVariant)
        Icon(if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore, if (expanded) "Hide device details" else "Show device details", modifier = Modifier.size(18.dp))
    }
    if (expanded) {
        SelectionContainer {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("Node $node", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                peer?.let {
                    it.attempts.forEach { attempt -> Text("${attempt.operation} · ${attempt.stage} · ${attempt.outcome ?: "in progress"} · ${attempt.endedAtEpochMillis?.let { at -> date(at.toLong() / 1000) } ?: "not ended"}", style = MaterialTheme.typography.labelSmall) }
                    it.link.openedAtEpochSeconds?.let { opened -> Text("Link opened: ${date(opened.toLong())}", style = MaterialTheme.typography.labelSmall) }
                    it.link.roundTripMillis?.let { rtt -> Text("Round trip: $rtt ms", style = MaterialTheme.typography.labelSmall) }
                    Text("Source: ${it.source} · selection: ${it.choice} · saved contact: ${it.isConfigured}", style = MaterialTheme.typography.labelSmall)
                    if (it.routes.isEmpty()) Text("No saved dialing route", style = MaterialTheme.typography.labelSmall)
                    it.routes.forEach { route -> Text("${route.transport}: ${route.address}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace) }
                }
            }
        }
    }
}
