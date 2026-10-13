package org.example.examples.shopping.presentation.screen.devices

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.presentation.component.KindChip
import org.example.examples.shopping.presentation.component.ResetShoppingButton
import org.example.examples.shopping.presentation.component.SyncBadge
import org.example.examples.shopping.presentation.component.SyncBadgeState
import org.example.examples.shopping.presentation.screen.sync.profile
import org.example.examples.shopping.util.ago
import org.example.examples.shopping.util.date
import rocky.ConnectionState
import rocky.LinkState

/** Local identity, connection controls and diagnostics in one card. */
@Composable
internal fun SelfDeviceCard(me: HouseholdDevice, state: DevicesState, now: Long, onOnline: (Boolean) -> Unit, onReset: () -> Unit) {
    var expanded by rememberSaveable(me.node) { mutableStateOf(false) }
    val readiness = state.readiness
    val self = state.self
    val connection = state.connection
    ElevatedCard(Modifier.fillMaxWidth(), colors = CardDefaults.elevatedCardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Filled.PhoneAndroid, null, tint = MaterialTheme.colorScheme.primary)
                Text(me.name, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                me.kind?.let { KindChip(it) }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Online", style = MaterialTheme.typography.labelLarge)
                Spacer(Modifier.width(8.dp))
                Switch(connection.state != ConnectionState.Offline, onOnline, Modifier.semantics { contentDescription = "Online" })
            }
            SelectionContainer {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("${readiness?.state?.name?.lowercase() ?: "starting"} · revision ${readiness?.revision ?: "…"} · ${self?.transport ?: "no transport"}", style = MaterialTheme.typography.bodySmall)
                    Text("Last sync ${ago(connection.atEpochMillis, now)} · ${connection.peers} peers reached", style = MaterialTheme.typography.labelSmall)
                    Text("Last transfer ${ago(state.lastTransferAtEpochMillis, now)}", style = MaterialTheme.typography.labelSmall)
                    connection.error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
                    readiness?.bootstrap?.let { progress ->
                        Text("Bootstrap ${progress.percent}% · ${progress.rowsDone}/${progress.rowsTotal} rows · ${progress.scopesDone}/${progress.scopesTotal} scopes", style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
            readiness?.bootstrap?.let { progress -> LinearProgressIndicator(progress = { progress.percent / 100f }, modifier = Modifier.fillMaxWidth()) }
            Row(Modifier.fillMaxWidth().clickable { expanded = !expanded }.padding(vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("Identity and access", Modifier.weight(1f), style = MaterialTheme.typography.labelMedium)
                Icon(if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore, if (expanded) "Hide local device details" else "Show local device details")
            }
            if (expanded) {
                SelectionContainer {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        me.kind?.let { Text(it.summary, style = MaterialTheme.typography.bodySmall) }
                        Text("Node ${me.node}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                        Text("Domain ${state.domain}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                        self?.let {
                            Text("Grants: ${profile(readiness?.profile ?: it.profile)} · ${readiness?.revocations?.statements ?: it.revocations.statements} revocations", style = MaterialTheme.typography.bodySmall)
                            Text("Credentials until ${date(readiness?.expiresAtEpochSeconds ?: it.expiresAtEpochSeconds)}", style = MaterialTheme.typography.labelSmall)
                            Text("Revocation digest: ${readiness?.revocations?.digest ?: it.revocations.digest}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                        }
                        readiness?.let { Text("Issuer route for renewal: ${if (it.issuerContact) "available" else "missing"}", style = MaterialTheme.typography.labelSmall) }
                        if (me.lists.isNotEmpty()) Text("Keys received: ${me.lists.joinToString()}", style = MaterialTheme.typography.bodySmall)
                    }
                }
                ResetShoppingButton(onReset)
            }
        }
    }
}
