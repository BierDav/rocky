package org.example.examples.shopping.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import rocky.ConnectionState

@Composable
fun SyncScreen(model: SyncViewModel) {
    val state by model.state.collectAsState()
    var now by remember { mutableStateOf(currentMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            now = currentMillis()
            delay(1_000)
        }
    }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        item { StatusCard(state, now, onOnline = model::setOnline, onSync = model::syncNow) }
        item { IdentityCard(state) }
        item { SectionTitle("Sync partners", "Known peers and Rocky's reason for choosing each one.") }
        items(state.peers, key = { it.node }) { peer ->
            OutlinedCard(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(12.dp)) {
                    Text(peer.node.take(12), style = MaterialTheme.typography.titleSmall)
                    Text("${peer.source} · ${peer.choice} · ${peer.state}", style = MaterialTheme.typography.bodySmall)
                    peer.error?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                }
            }
        }
        item { SectionTitle("Lists on this device", "Each list is one partition group: one secret, one key epoch per table, held or not.") }
        items(state.holdings, key = { it.list.id }) { holding -> HoldingCard(holding) }
        if (state.holdings.isEmpty()) item { Text("No list is held here yet.", style = MaterialTheme.typography.bodySmall) }
        item { SectionTitle("Activity", "What Rocky did in the background, newest first.") }
        items(state.entries, key = { it.key }) { entry -> EntryRow(entry, now) }
    }
}

@Composable
private fun StatusCard(state: SyncState, now: Long, onOnline: (Boolean) -> Unit, onSync: () -> Unit) {
    val connection = state.connection
    val online = connection.state != ConnectionState.Offline
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                SyncBadge(state, onClick = onSync)
                Spacer(Modifier.weight(1f))
                Text("Online", style = MaterialTheme.typography.labelLarge)
                Spacer(Modifier.width(8.dp))
                Switch(online, onOnline, Modifier.semantics { contentDescription = "Online" })
            }
            Fact("State", state.readiness?.state?.name?.lowercase() ?: "…")
            Fact("Last sync", "${ago(connection.atEpochMillis, now)} · ${connection.peers} peer${if (connection.peers == 1) "" else "s"} reached")
            Fact("Last transfer", ago(state.lastTransferAtEpochMillis, now))
            connection.error?.let { Fact("Error", it) }
            Fact("Revision", state.readiness?.revision?.toString() ?: "…")
            OutlinedButton(onSync, enabled = online) {
                Icon(Icons.Filled.Sync, null)
                Spacer(Modifier.width(6.dp))
                Text("Sync now")
            }
        }
    }
}

@Composable
private fun IdentityCard(state: SyncState) {
    val self = state.self
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("This device", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                state.kind?.let { KindChip(it) }
            }
            Fact("Node", self?.node?.take(24)?.plus("…") ?: "…", mono = true)
            Fact("Grants", self?.profile?.let(::profile) ?: "…")
            Fact("Revocations", state.readiness?.revocations?.statements?.toString() ?: "…")
            Fact("Credentials expire", state.readiness?.expiresAtEpochSeconds?.let { "in ${(it * 1000 - currentMillis()) / 86_400_000} days" } ?: "…")
            Fact("Transport", self?.transport ?: "…")
            Fact("Domain", state.domain.take(48) + if (state.domain.length > 48) "…" else "", mono = true)
        }
    }
}

@Composable
private fun HoldingCard(holding: ListHolding) {
    val status = holding.status
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(holding.list.title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                Text(
                    if (status?.isKeyed == true) "secret epoch ${status.epochs.maxOrNull()}" else "no key",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            status?.tables?.forEach { table ->
                Text(
                    "${table.table}: ${table.holding ?: "not subscribed"} · key epochs ${table.keyEpochs.joinToString().ifEmpty { "none" }} · ${table.partition.take(8)}",
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
        }
    }
}

@Composable
private fun EntryRow(entry: ActivityEntry, now: Long) {
    val (icon, tint) = entry.kind.look()
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.Top) {
        Icon(icon, null, Modifier.size(20.dp), tint = tint)
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(entry.title, style = MaterialTheme.typography.bodyMedium)
            entry.detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        Text(ago(entry.atEpochMillis, now), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun EntryKind.look(): Pair<ImageVector, Color> = when (this) {
    EntryKind.Transfer -> Icons.Filled.SwapVert to Color(0xFF43A047)
    EntryKind.Checkpoint -> Icons.Filled.Inventory2 to Color(0xFFF9A825)
    EntryKind.Horizon -> Icons.Filled.ContentCut to Color(0xFF8E24AA)
    EntryKind.PeerUp -> Icons.Filled.Link to Color(0xFF43A047)
    EntryKind.PeerDown -> Icons.Filled.LinkOff to Color(0xFFE53935)
    EntryKind.Credentials -> Icons.Filled.Badge to MaterialTheme.colorScheme.tertiary
    EntryKind.Device -> Icons.Filled.Devices to MaterialTheme.colorScheme.primary
    EntryKind.Author -> Icons.Filled.Draw to MaterialTheme.colorScheme.secondary
    EntryKind.Key -> Icons.Filled.Key to Color(0xFF00897B)
    EntryKind.Network -> Icons.Filled.Wifi to MaterialTheme.colorScheme.onSurfaceVariant
    EntryKind.Problem -> Icons.Filled.ErrorOutline to MaterialTheme.colorScheme.error
}

@Composable
private fun Fact(label: String, value: String, mono: Boolean = false) {
    Row {
        Text(label, Modifier.width(140.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, style = MaterialTheme.typography.bodySmall, fontFamily = if (mono) FontFamily.Monospace else null)
    }
}
