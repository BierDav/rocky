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
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.Approval
import org.example.examples.shopping.Coordinated
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/**
 * Purchase approvals as strong commands: any device asks, a trusted device approves or declines, and the coordination
 * service orders every request and decision, so the first decision stands on every device.
 */
@Composable
fun ApprovalsScreen(model: ApprovalsViewModel) {
    val state by model.state.collectAsState()
    var asking by remember { mutableStateOf(false) }
    val available = state.status !is Coordinated.Unavailable
    Box(Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            item { CoordinationCard(state.status, state.trusted) }
            val (pending, decided) = state.approvals.partition { it.approved == null }
            item {
                SectionTitle(
                    "Waiting for a decision",
                    if (state.trusted) {
                        "Approve or decline. If another trusted device decides at the same time, the coordinator keeps the first decision and refuses the other."
                    } else {
                        "A trusted device decides; the result reaches this device with sync."
                    },
                )
            }
            if (state.asking > 0) item { Sending("Sending your request to an executing trusted device…") }
            if (pending.isEmpty() && state.asking == 0) item { Hint("No open requests") }
            items(pending, key = { it.key }) { approval ->
                ApprovalCard(
                    approval,
                    decidable = state.trusted && available,
                    busy = approval.key in state.deciding,
                    onDecide = { model.decide(approval.key, it) },
                )
            }
            if (decided.isNotEmpty()) {
                item { SectionTitle("Decided") }
                items(decided, key = { it.key }) { ApprovalCard(it, decidable = false, busy = false, onDecide = {}) }
            }
            item { Spacer(Modifier.height(72.dp)) }
        }
        if (available) {
            ExtendedFloatingActionButton(
                onClick = { asking = true },
                icon = { Icon(Icons.Filled.AddShoppingCart, null) },
                text = { Text("Ask to buy") },
                modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
            )
        }
    }
    if (asking) {
        AskDialog(onDismiss = { asking = false }) { title, note ->
            asking = false
            model.ask(title, note)
        }
    }
}

@Composable
private fun CoordinationCard(status: Coordinated, trusted: Boolean) {
    val (color, title, detail) = when (status) {
        Coordinated.Unavailable -> Triple(
            Color(0xFF9E9E9E),
            "No coordination service",
            "This device opened without one, as over direct TCP. Requests need the public coordinator, reached over iroh.",
        )

        Coordinated.Member -> Triple(
            Color(0xFF43A047),
            "Coordinated",
            "Requests go to a trusted device that executes them; the public coordination service orders them.",
        )

        is Coordinated.Waiting -> Triple(Color(0xFFF9A825), "Setting up coordination", status.reason)

        is Coordinated.Registered -> Triple(
            Color(0xFF43A047),
            "Registered with the coordination service",
            "Control revision ${status.revision}: ${status.executors} trusted device(s) execute, ${status.invokers} device(s) may ask. " +
                "Renewed automatically before ${date(status.expiresAtEpochSeconds)}.",
        )

        is Coordinated.Failed -> Triple(MaterialTheme.colorScheme.error, "Coordination problem", status.message)
    }
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Filled.Gavel, null, tint = color)
                Spacer(Modifier.width(12.dp))
                Text(title, style = MaterialTheme.typography.titleMedium)
            }
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (trusted) {
                Text(
                    "The coordinator sees only request identities and commitments, never titles or decisions.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun ApprovalCard(approval: Approval, decidable: Boolean, busy: Boolean, onDecide: (Boolean) -> Unit) {
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(approval.title, style = MaterialTheme.typography.titleMedium)
                    if (approval.note.isNotBlank()) Text(approval.note, style = MaterialTheme.typography.bodyMedium)
                    Text("Asked by ${approval.requester}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                when (approval.approved) {
                    true -> Verdict(Icons.Filled.CheckCircle, Color(0xFF43A047), "Approved")
                    false -> Verdict(Icons.Filled.Cancel, MaterialTheme.colorScheme.error, "Declined")
                    null -> Verdict(Icons.Filled.HourglassTop, Color(0xFFF9A825), "Pending")
                }
            }
            approval.decider?.let { Text("Decided by $it", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            if (decidable) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (busy) {
                        Sending("Ordering the decision…")
                    } else {
                        Button({ onDecide(true) }) {
                            Icon(Icons.Filled.Check, null)
                            Spacer(Modifier.width(6.dp))
                            Text("Approve")
                        }
                        OutlinedButton({ onDecide(false) }) {
                            Icon(Icons.Filled.Close, null)
                            Spacer(Modifier.width(6.dp))
                            Text("Decline")
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Verdict(icon: androidx.compose.ui.graphics.vector.ImageVector, color: Color, label: String) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Icon(icon, null, tint = color)
        Text(label, style = MaterialTheme.typography.labelSmall, color = color)
    }
}

@Composable
private fun Sending(text: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        Spacer(Modifier.width(8.dp))
        Text(text, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun Hint(text: String) = Text(text, Modifier.padding(horizontal = 4.dp, vertical = 8.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)

@Composable
private fun AskDialog(onDismiss: () -> Unit, onAsk: (String, String) -> Unit) {
    var title by remember { mutableStateOf("") }
    var note by remember { mutableStateOf("") }
    AlertDialog(
        onDismiss,
        title = { Text("Ask to buy") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "A trusted device approves or declines it. The request is a strong command: it counts once its order is confirmed.",
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedTextField(title, { title = it }, label = { Text("What, e.g. Coffee machine") }, singleLine = true)
                OutlinedTextField(note, { note = it }, label = { Text("Why or how much (optional)") })
            }
        },
        confirmButton = { Button({ onAsk(title, note) }, enabled = title.isNotBlank()) { Text("Ask") } },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
    )
}

/** A local date, such as a deadline or an expiry. */
internal fun date(epochSeconds: Long): String = DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT).withZone(ZoneId.systemDefault()).format(Instant.ofEpochSecond(epochSeconds))
