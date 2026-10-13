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
import org.example.examples.shopping.presentation.component.bootstrapPercent
import org.example.examples.shopping.presentation.screen.sync.profile
import org.example.examples.shopping.util.ago
import org.example.examples.shopping.util.date

/** Local identity, connection controls and diagnostics in one card. */
@Composable
internal fun SelfDeviceCard(me: HouseholdDevice, state: DevicesState, now: Long, onOnline: (Boolean) -> Unit, onReset: () -> Unit) {
    var expanded by rememberSaveable(me.node) { mutableStateOf(false) }
    val snapshot = state.snapshot
    val authority = snapshot?.authority
    val data = snapshot?.data
    val self = state.self
    val report = snapshot?.network?.lastReconciliation
    ElevatedCard(Modifier.fillMaxWidth(), colors = CardDefaults.elevatedCardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Icon(Icons.Filled.PhoneAndroid, null, tint = MaterialTheme.colorScheme.primary)
                Text(me.name, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                me.kind?.let { KindChip(it) }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Networking enabled", style = MaterialTheme.typography.labelLarge)
                Spacer(Modifier.width(8.dp))
                Switch(snapshot?.network?.networkingEnabled == true, onOnline, Modifier.semantics { contentDescription = "Networking enabled" }, enabled = snapshot != null)
            }
            SelectionContainer {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("revision ${data?.revision ?: "…"} · ${self?.transport ?: "no transport"}", style = MaterialTheme.typography.bodySmall)
                    Text("Last reconciliation ${ago(report?.endedAtEpochMillis?.toLong(), now)} · ${report?.peers?.count { it.skip == null } ?: 0} peers attempted", style = MaterialTheme.typography.labelSmall)
                    Text("Last transfer ${ago(state.lastTransferAtEpochMillis, now)}", style = MaterialTheme.typography.labelSmall)
                    authority?.let {
                        if (it.verifiedRevocation != null) Text("Verified revocation ${it.verifiedRevocation?.id}", color = MaterialTheme.colorScheme.error)
                        if (it.expired) Text("Credentials expired", color = MaterialTheme.colorScheme.error)
                        if (it.loggedOut) Text("Logged out", color = MaterialTheme.colorScheme.error)
                    }
                    data?.bootstrap?.let { progress ->
                        Text("Bootstrap ${bootstrapPercent(progress)}% · ${progress.rowsDone}/${progress.rowsTotal} rows · ${progress.scopesDone}/${progress.scopesTotal} scopes", style = MaterialTheme.typography.labelSmall)
                    }
                }
            }
            data?.bootstrap?.let { progress -> LinearProgressIndicator(progress = { bootstrapPercent(progress) / 100f }, modifier = Modifier.fillMaxWidth()) }
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
                            Text("Grants: ${authority?.profile?.let { profile -> "${profile.kind}: ${profile.scopes.joinToString()}" } ?: profile(it.profile)} · ${authority?.revocations?.statements ?: it.revocations.statements} revocations", style = MaterialTheme.typography.bodySmall)
                            Text("Credentials until ${date(authority?.expiresAtEpochSeconds?.toLong() ?: it.expiresAtEpochSeconds)}", style = MaterialTheme.typography.labelSmall)
                            Text("Revocation digest: ${authority?.revocations?.digest ?: it.revocations.digest}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                        }
                        authority?.let { Text("Known renewal routes: ${it.renewalRoutes.size}", style = MaterialTheme.typography.labelSmall) }
                        snapshot?.let {
                            Text("Attachment: ${it.network.attachment} · worker enabled: ${it.network.workerEnabled} · running: ${it.network.workerRunning}", style = MaterialTheme.typography.labelSmall)
                            Text("Coordinator: ${it.coordination.mode}", style = MaterialTheme.typography.labelSmall)
                            it.coordination.attempts.forEach { attempt -> Text("${attempt.operation}: ${attempt.outcome ?: attempt.stage} ${attempt.failure?.detail.orEmpty()}", style = MaterialTheme.typography.labelSmall) }
                            val relays = it.network.relay.relays
                            if (relays == null) {
                                Text("Home relay status not observed", style = MaterialTheme.typography.labelSmall)
                            } else {
                                relays.forEach { relay -> Text("${relay.url}: connected=${relay.connected} ${relay.authDeniedReason ?: relay.lastError.orEmpty()}", style = MaterialTheme.typography.labelSmall) }
                            }
                        }
                        if (me.lists.isNotEmpty()) Text("Keys received: ${me.lists.joinToString()}", style = MaterialTheme.typography.bodySmall)
                    }
                }
                ResetShoppingButton(onReset)
            }
        }
    }
}
