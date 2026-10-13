package org.example.examples.shopping.presentation.component

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
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
import rocky.LinkPath
import rocky.LinkState
import rocky.PeerLink
import kotlin.math.roundToLong

/**
 * An open link's network paths as the transport measured them at the last status read. Collapsed, one line names
 * the selected path and its round trip; expanded, every path is listed, and each expands to its counters.
 */
@Composable
internal fun LinkPaths(link: PeerLink, modifier: Modifier = Modifier) {
    if (link.state != LinkState.Open || link.paths.isEmpty()) return
    var expanded by rememberSaveable { mutableStateOf(false) }
    val selected = link.paths.firstOrNull { it.selected }
    Column(modifier.fillMaxWidth().animateContentSize()) {
        Row(
            Modifier.fillMaxWidth().clickable { expanded = !expanded }.padding(vertical = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                buildString {
                    append(link.paths.size).append(if (link.paths.size == 1) " path" else " paths")
                    selected?.let { append(" · ").append(it.kind).append(" · ").append(millis(it.roundTripMicros.toLong())) }
                },
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            Icon(
                if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                if (expanded) "Hide network paths" else "Show network paths",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (expanded) {
            Column(Modifier.padding(top = 4.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                link.paths.forEach { path -> PathRow(path) }
            }
        }
    }
}

@Composable
private fun PathRow(path: LinkPath) {
    var expanded by rememberSaveable(path.kind, path.remote) { mutableStateOf(false) }
    val tint = if (path.kind == "direct") Color(0xFF43A047) else Color(0xFFF9A825)
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = if (path.selected) 0.7f else 0.35f),
        modifier = Modifier.fillMaxWidth().animateContentSize().clickable { expanded = !expanded },
    ) {
        Column(Modifier.padding(horizontal = 10.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(8.dp).background(tint, CircleShape))
                Spacer(Modifier.width(6.dp))
                Text(
                    path.kind.replaceFirstChar(Char::uppercase) + if (path.selected) " · selected" else "",
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.weight(1f),
                )
                Text(millis(path.roundTripMicros.toLong()), style = MaterialTheme.typography.labelLarge, fontFamily = FontFamily.Monospace)
                Icon(
                    if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                    if (expanded) "Hide path counters" else "Show path counters",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.size(18.dp),
                )
            }
            Mono("to ${path.remote}")
            path.local?.let { Mono("from $it") }
            if (expanded) {
                Spacer(Modifier.height(4.dp))
                Fact("Round trip", "${millis(path.roundTripMicros.toLong())} smoothed")
                Fact("MTU", "${path.mtu} B")
                Fact("Congestion window", bytes(path.window.toLong()))
                Fact("Sent", "${bytes(path.sentBytes.toLong())} in ${path.sentDatagrams} datagrams")
                Fact("Received", "${bytes(path.receivedBytes.toLong())} in ${path.receivedDatagrams} datagrams")
                Fact("Lost", "${path.lostPackets} packets · ${bytes(path.lostBytes.toLong())} · ${lossRate(path)}")
                Fact("Congestion events", path.congestionEvents.toString())
            }
        }
    }
}

@Composable
private fun Mono(text: String) = Text(
    text,
    style = MaterialTheme.typography.labelSmall,
    fontFamily = FontFamily.Monospace,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
)

@Composable
private fun Fact(label: String, value: String) {
    Row {
        Text(label, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(1f))
        Text(value, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
    }
}

internal fun millis(micros: Long): String = "${decimal(micros / 1_000.0)} ms"

internal fun bytes(count: Long): String = when {
    count >= 1L shl 30 -> "${decimal(count / (1L shl 30).toDouble())} GiB"
    count >= 1L shl 20 -> "${decimal(count / (1L shl 20).toDouble())} MiB"
    count >= 1L shl 10 -> "${decimal(count / (1L shl 10).toDouble())} KiB"
    else -> "$count B"
}

private fun lossRate(path: LinkPath): String = if (path.sentDatagrams == 0uL) "no datagrams sent" else "${decimal(path.lostPackets.toDouble() * 100.0 / path.sentDatagrams.toDouble())} % of sent"

/** One decimal place; common Kotlin has no `String.format`. */
private fun decimal(value: Double): String {
    val tenths = (value * 10).roundToLong()
    return "${tenths / 10}.${tenths % 10}"
}
