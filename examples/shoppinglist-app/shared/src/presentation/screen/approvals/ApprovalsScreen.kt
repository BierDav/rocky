package org.example.examples.shopping.presentation.screen.approvals

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
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.data.coordination.Approval
import org.example.examples.shopping.data.coordination.Coordinated
import org.example.examples.shopping.presentation.component.SectionTitle
import org.example.examples.shopping.presentation.component.ShowMessages
import org.example.examples.shopping.util.date

@Serializable
data object ApprovalsKey : NavKey

/**
 * Purchase requests sync as ordinary rows; trusted devices coordinate decisions so the first committed one stands.
 */
@Composable
fun ApprovalsScreen(model: ApprovalsViewModel = metroViewModel()) {
    ShowMessages(model.messages)
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
            if (state.asking > 0) item { Sending("Saving your request…") }
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
        ExtendedFloatingActionButton(
            onClick = { asking = true },
            icon = { Icon(Icons.Filled.AddShoppingCart, null) },
            text = { Text("Ask to buy") },
            modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
        )
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
            "Requests can be saved and synced. A coordination service must be configured before trusted devices can decide.",
        )

        Coordinated.Member -> Triple(
            Color(0xFF43A047),
            "Coordinated",
            "Requests sync to trusted devices, and committed decisions sync back.",
        )

        is Coordinated.Waiting -> Triple(Color(0xFFF9A825), "Setting up coordination", status.reason)

        is Coordinated.Registered -> Triple(
            Color(0xFF43A047),
            "Registered with the coordination service",
            "${status.trusted} trusted device(s) can decide. Every member may ask. " +
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
                    "Your request is saved here and syncs to trusted devices. Their first committed decision stands.",
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
