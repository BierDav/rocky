package org.example.examples.shopping.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import org.example.examples.pairing.QrCode
import org.example.examples.shopping.NodeKind
import org.example.examples.shopping.PairingCode
import rocky.SessionStatus

@Composable
fun DevicesScreen(model: DevicesViewModel) {
    val state by model.state.collectAsState()
    val busy by model.revoking.collectAsState()
    val contact by model.contact.collectAsState()
    var meeting by remember { mutableStateOf(false) }
    var adding by remember { mutableStateOf(false) }
    var sharing by remember { mutableStateOf<HouseholdDevice?>(null) }
    var revoking by remember { mutableStateOf<HouseholdDevice?>(null) }
    var forgetting by remember { mutableStateOf<HouseholdDevice?>(null) }
    var showRevoked by remember { mutableStateOf(false) }
    var showing by remember { mutableStateOf<PairingCode?>(null) }
    if (meeting) {
        ContactDialog("Meet a peer", { meeting = false }) { code, label ->
            meeting = false
            model.rememberContact(code, label)
        }
    }
    contact?.let { code ->
        AlertDialog(
            { model.contact.value = null },
            confirmButton = { TextButton({ model.contact.value = null }) { Text("Done") } },
            title = { Text("My contact code") },
            text = {
                Column {
                    QrCode(code, Modifier.size(240.dp))
                    SelectionContainer { Text(code) }
                }
            },
        )
    }
    Box(Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            state.me?.let { me ->
                item { SectionTitle("This device") }
                item { SelfCard(me, state.self) }
            }
            item { SectionTitle(if (state.trusted) "Household" else "Peers met", "Contacts and labels stay on this device.") }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(model::showContact) { Text("My contact code") }
                    OutlinedButton({ meeting = true }) { Text("Scan contact") }
                }
            }
            if (state.devices.isEmpty()) {
                item {
                    Text(
                        if (state.trusted) "No other device yet: add one below." else "No other device is known here yet.",
                        Modifier.padding(4.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            items(state.devices, key = { it.node }) { device ->
                DeviceCard(
                    device,
                    canShare = state.trusted && device.kind == NodeKind.Partial,
                    busy = device.node in busy,
                    onShare = { sharing = device },
                    onRevoke = { revoking = device },
                )
            }
            if (state.invitations.isNotEmpty()) {
                item { SectionTitle("Open invitations", "Each trusted device has at most one: a new code replaces its last. A code works once, until it expires.") }
                items(state.invitations, key = { "invitation-" + it.code.invitation }) { open -> InvitationCard(open, onShow = { showing = open.code }) }
            }
            if (state.revoked.isNotEmpty()) {
                item { RevokedHeader(state.revoked.size, showRevoked) { showRevoked = !showRevoked } }
                if (showRevoked) {
                    items(state.revoked, key = { "revoked-" + it.node }) { device ->
                        DeviceCard(
                            device,
                            canShare = false,
                            busy = false,
                            onShare = {},
                            onRevoke = {},
                            onForget = if (state.trusted) ({ forgetting = device }) else null,
                        )
                    }
                }
            }
            state.network?.let { network ->
                item {
                    SectionTitle("Network", "${network.kind ?: "no"} transport, ${if (network.owned) "owned by Rocky" else "owned by the app"}")
                    Text(
                        "Inbound sessions: ${network.inboundActive} of ${network.inboundLimit} active, ${network.inboundServed} served, ${network.inboundRefused} refused",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            item { Spacer(Modifier.height(72.dp)) }
        }
        if (state.trusted) {
            ExtendedFloatingActionButton(
                onClick = { adding = true },
                icon = { Icon(Icons.Filled.QrCode2, null) },
                text = { Text("Add device") },
                modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
            )
        }
    }
    showing?.let { code ->
        AlertDialog(
            { showing = null },
            confirmButton = { TextButton({ showing = null }) { Text("Done") } },
            title = { Text("Scan on ${code.name}") },
            text = { Column(Modifier.verticalScroll(rememberScrollState())) { InvitationCode(code) } },
        )
    }
    if (adding) {
        AddDeviceDialog(state.invitation, onInvite = model::invite, onDismiss = {
            adding = false
            model.dismissInvitation()
        })
    }
    revoking?.let { device ->
        AlertDialog(
            onDismissRequest = { revoking = null },
            title = { Text("Revoke ${device.name}?") },
            text = { Text("This device will lose access to future sync and new list keys. Data already stored on it stays there.") },
            confirmButton = {
                TextButton({
                    model.revoke(device.node)
                    revoking = null
                }) { Text("Revoke device") }
            },
            dismissButton = { TextButton({ revoking = null }) { Text("Cancel") } },
        )
    }
    forgetting?.let { device ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Clear ${device.name}'s name?") },
            text = {
                Text(
                    "Clears its application name. The registry tombstone and signed revocation remain, so the old identity stays revoked.",
                )
            },
            confirmButton = {
                TextButton({
                    model.forget(device.node)
                    forgetting = null
                }) { Text("Remove") }
            },
            dismissButton = { TextButton({ forgetting = null }) { Text("Cancel") } },
        )
    }
    sharing?.let { device ->
        AlertDialog(
            { sharing = null },
            confirmButton = { TextButton({ sharing = null }) { Text("Done") } },
            title = { Text("Share with ${device.name}") },
            text = {
                Column {
                    Text("A partial device holds only the lists sent to it. Its key travels sealed to it alone, directly or through the custody peer.")
                    state.lists.forEach { list ->
                        val has = list.title in device.lists
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(has, { if (!has) model.share(list.id, device.node) }, enabled = !has)
                            Text(list.title)
                        }
                    }
                }
            },
        )
    }
}

/** This device: who it is to the household and the authority it holds. */
@Composable
private fun SelfCard(me: HouseholdDevice, self: SessionStatus?) {
    ElevatedCard(Modifier.fillMaxWidth(), colors = CardDefaults.elevatedCardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Filled.PhoneAndroid, null, tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(12.dp))
                Text(me.name, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                me.kind?.let { KindChip(it) }
            }
            me.kind?.let { Text(it.summary, style = MaterialTheme.typography.bodySmall) }
            SelectionContainer {
                Text("Node ${me.node}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
            }
            self?.let { session ->
                Text(
                    "${session.transport} · ${session.revocations.statements} revocations · credentials until ${date(session.expiresAtEpochSeconds)}",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (me.lists.isNotEmpty()) Text("Keys received: ${me.lists.joinToString()}", style = MaterialTheme.typography.bodySmall)
        }
    }
}

@Composable
private fun RevokedHeader(count: Int, expanded: Boolean, onToggle: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onToggle).padding(vertical = 8.dp, horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Filled.Block, null, tint = MaterialTheme.colorScheme.error)
        Spacer(Modifier.width(8.dp))
        Text("Revoked devices ($count)", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
        Icon(if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore, if (expanded) "Collapse" else "Expand")
    }
}

@Composable
private fun DeviceCard(
    device: HouseholdDevice,
    canShare: Boolean,
    busy: Boolean,
    onShare: () -> Unit,
    onRevoke: () -> Unit,
    onForget: (() -> Unit)? = null,
) {
    val peer = device.peer
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(if (device.isSelf) Icons.Filled.PhoneAndroid else Icons.Filled.Devices, null, tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(device.name + if (device.isSelf) " (this device)" else "", style = MaterialTheme.typography.titleMedium)
                    Text(
                        device.node.take(16) + "…",
                        style = MaterialTheme.typography.labelSmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                device.kind?.let { KindChip(it) }
            }
            when {
                device.revoked -> Status(MaterialTheme.colorScheme.error, "Revoked · future access removed")
                device.isSelf -> {}
                peer == null -> Status(Color(0xFF9E9E9E), "Syncs through other devices")
                peer.state == "connected" -> Status(Color(0xFF43A047), "Connected ${ago(peer.checkedAtEpochSeconds?.times(1000))}" + if (peer.isIssuer) " · issuer" else "")
                peer.state == "unreachable" -> Status(Color(0xFFE53935), "Unreachable: ${peer.error ?: "no answer"}")
                else -> Status(Color(0xFF9E9E9E), "Not contacted yet")
            }
            peer?.routes?.firstOrNull()?.let { route ->
                Text("via ${route.transport}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (device.lists.isNotEmpty()) Text("Keys sent: ${device.lists.joinToString()}", style = MaterialTheme.typography.bodySmall)
            Row {
                if (canShare) {
                    TextButton(onShare, enabled = !busy) {
                        Icon(Icons.Filled.Share, null)
                        Spacer(Modifier.width(6.dp))
                        Text("Share lists")
                    }
                }
                if (device.revocable) {
                    TextButton(onRevoke, enabled = !busy) {
                        Icon(Icons.Filled.Block, null)
                        Spacer(Modifier.width(6.dp))
                        Text(if (busy) "Revoking…" else "Revoke device")
                    }
                }
                onForget?.let {
                    TextButton(it) {
                        Icon(Icons.Filled.DeleteForever, null)
                        Spacer(Modifier.width(6.dp))
                        Text("Clear name")
                    }
                }
            }
        }
    }
}

@Composable
internal fun KindChip(kind: NodeKind) {
    val color = when (kind) {
        NodeKind.Trusted -> MaterialTheme.colorScheme.primaryContainer
        NodeKind.Full -> MaterialTheme.colorScheme.secondaryContainer
        NodeKind.Partial -> MaterialTheme.colorScheme.tertiaryContainer
    }
    Surface(color = color, shape = MaterialTheme.shapes.small) {
        Text(kind.label, Modifier.padding(horizontal = 8.dp, vertical = 2.dp), style = MaterialTheme.typography.labelMedium)
    }
}

@Composable
private fun Status(color: Color, text: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(8.dp).background(color, CircleShape))
        Spacer(Modifier.width(6.dp))
        Text(text, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun AddDeviceDialog(invitation: PairingCode?, onInvite: (NodeKind, String) -> Unit, onDismiss: () -> Unit) {
    var kind by remember { mutableStateOf(NodeKind.Full) }
    var name by remember { mutableStateOf("") }
    AlertDialog(
        onDismiss,
        confirmButton = {
            if (invitation == null) {
                Button({ onInvite(kind, name) }) { Text("Show code") }
            } else {
                TextButton(onDismiss) { Text("Done") }
            }
        },
        dismissButton = { if (invitation == null) TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(if (invitation == null) "Add a device" else "Scan on ${invitation.name}") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (invitation == null) {
                    OutlinedTextField(name, { name = it }, label = { Text("Name, e.g. Pixel") }, singleLine = true)
                    NodeKind.entries.forEach { option ->
                        Row(
                            Modifier.fillMaxWidth().selectable(kind == option, onClick = { kind = option }).padding(vertical = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(kind == option, { kind = option })
                            Column {
                                Text(option.label, style = MaterialTheme.typography.titleSmall)
                                Text(option.summary, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                    }
                } else {
                    InvitationCode(invitation)
                }
            }
        },
    )
}

/** The QR code of [invitation], with what it does and a way to copy it. */
@Composable
private fun ColumnScope.InvitationCode(invitation: PairingCode) {
    @Suppress("DEPRECATION") // LocalClipboard needs a platform ClipEntry.
    val clipboard = LocalClipboardManager.current
    val code = invitation.toString()
    QrCode(code, Modifier.size(280.dp).align(Alignment.CenterHorizontally))
    Text(
        "A ${invitation.kind.label.lowercase()} device. The code works once, for 10 minutes; it reaches the inviting device over iroh, also from a cellular network. " +
            "A phone's camera app opens it in Shopping.",
        style = MaterialTheme.typography.bodySmall,
    )
    SelectionContainer { Text(code.take(48) + "…", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace) }
    TextButton({ clipboard.setText(AnnotatedString(code)) }) {
        Icon(Icons.Filled.ContentCopy, null)
        Spacer(Modifier.width(6.dp))
        Text("Copy code")
    }
}

@Composable
private fun InvitationCard(open: OpenInvitation, onShow: () -> Unit) {
    var now by remember { mutableStateOf(currentMillis() / 1000) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1_000)
            now = currentMillis() / 1000
        }
    }
    val left = (open.expiresAtEpochSeconds - now).coerceAtLeast(0)
    OutlinedCard(Modifier.fillMaxWidth().clickable(onClick = onShow)) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.QrCode2, null, tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(open.code.name, style = MaterialTheme.typography.titleSmall)
                Text(
                    "By ${if (open.mine) "this device" else open.issuer} · expires in ${left / 60}:${(left % 60).toString().padStart(2, '0')}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            KindChip(open.code.kind)
        }
    }
}

@Composable
internal fun SectionTitle(title: String, detail: String? = null) {
    Column(Modifier.padding(top = 8.dp, bottom = 4.dp)) {
        Text(title, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
        detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
}
