package org.example.examples.shopping.presentation.screen.history

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.data.tips.MAIN
import org.example.examples.shopping.data.tips.SealOnPath
import org.example.examples.shopping.presentation.component.SectionTitle
import org.example.examples.shopping.presentation.component.ShowMessages
import org.example.examples.shopping.util.formatTimestamp
import rocky.HeldChange

@Serializable
data object HistoryKey : NavKey

/** The bookmark selector, seals and kept states, and held changes; see `docs/runtime/shopping-seals.md`. */
@Composable
fun HistoryScreen(model: HistoryViewModel = metroViewModel()) {
    ShowMessages(model.messages)
    val state by model.state.collectAsState()
    val view by model.view.collectAsState()
    val tips = state.tips
    if (tips == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        item { SectionTitle("Bookmark", "Devices sync only with devices following the same bookmark.") }
        item {
            LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                items(tips.bookmarks, key = { it.name }) { bookmark ->
                    FilterChip(bookmark.followed, { if (!bookmark.followed) model.follow(bookmark.name) }, label = { Text(bookmark.name) })
                }
            }
        }
        tips.pendingFollow?.let { item { Text("Switching to $it once its starting state arrives", style = MaterialTheme.typography.bodySmall) } }
        if (tips.losers.isNotEmpty()) {
            item { Text("Lost races: ${tips.losers.joinToString { "${it.name} → ${it.tip?.take(8) ?: "deleted"}" }}", style = MaterialTheme.typography.bodySmall) }
        }
        item { BranchControls(tips.bookmark, state.trusted, model) }

        item { SectionTitle("Seals", "Trusted devices seal $MAIN every 100 writes; every device then compacts.") }
        item {
            Text("Tip ${tips.tip.take(12)} · ${tips.operationsSinceSeal} writes since the last seal", style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
            val compaction = tips.compaction
            Text(
                "Compacted at ${compaction.compacted?.take(8) ?: "none"} · finalized ${compaction.finalized?.take(8) ?: "none"}" +
                    (compaction.waiting?.let { " · waiting: $it" } ?: ""),
                style = MaterialTheme.typography.bodySmall,
            )
            state.problem?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        }
        if (state.trusted) item { OutlinedButton(model::seal) { Text("Seal now") } }
        if (tips.path.isEmpty()) item { Text("Nothing sealed yet.", style = MaterialTheme.typography.bodySmall) }
        items(tips.path, key = { it.id }) { seal -> SealRow(seal, state.trusted && tips.bookmark == MAIN, model) }

        item { SectionTitle("Held changes", "Writes that could not cross a schema change or a rollback. Re-author them as new writes, or drop them.") }
        if (tips.held.isEmpty()) item { Text("None.", style = MaterialTheme.typography.bodySmall) }
        items(tips.held, key = { it.id }) { change -> HeldRow(change, model) }
    }
    view?.let { shown ->
        AlertDialog(
            onDismissRequest = model::closeView,
            confirmButton = { TextButton(model::closeView) { Text("Close") } },
            title = { Text("As sealed at ${shown.seal.take(8)}") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (shown.lists.isEmpty()) Text("No lists.")
                    shown.lists.forEach { list ->
                        Text(list.title, style = MaterialTheme.typography.titleSmall)
                        list.items.forEach { Text("${if (it.completed) "☑" else "☐"} ${it.title}", style = MaterialTheme.typography.bodySmall) }
                    }
                }
            },
        )
    }
}

@Composable
private fun BranchControls(followed: String?, trusted: Boolean, model: HistoryViewModel) {
    var name by rememberSaveable { mutableStateOf("dev") }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (followed != null && followed != MAIN) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton({ model.leave(followed, delete = false) }) { Text("Back to $MAIN and drop $followed") }
                if (trusted) OutlinedButton({ model.leave(followed, delete = true) }) { Text("Back to $MAIN and delete $followed") }
            }
        }
        if (trusted) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(name, { name = it }, label = { Text("Branch name") }, singleLine = true, modifier = Modifier.weight(1f))
                Button({ model.branch(name) }, enabled = name.isNotBlank() && name.trim() != MAIN) { Text("Open branch") }
            }
        }
    }
}

@Composable
private fun SealRow(seal: SealOnPath, rollBack: Boolean, model: HistoryViewModel) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Column(Modifier.weight(1f)) {
            Text(seal.id.take(12), fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodyMedium)
            Text(
                listOfNotNull(
                    seal.held?.name?.lowercase() ?: "shallow",
                    "final".takeIf { seal.final },
                    "closed".takeIf { seal.closed },
                ).joinToString(" · "),
                style = MaterialTheme.typography.labelSmall,
            )
        }
        if (seal.held != null) TextButton({ model.show(seal.id) }) { Text("View") }
        if (rollBack && seal.held != null) TextButton({ model.rollBack(seal.id) }) { Text("Roll back here") }
    }
}

@Composable
private fun HeldRow(change: HeldChange, model: HistoryViewModel) {
    ElevatedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(formatTimestamp(change.timeMicros / 1000), style = MaterialTheme.typography.labelSmall)
            change.rows.forEach { row ->
                val title = row.fields.firstOrNull { it.name == "title" || it.name == "name" }?.value
                Text("${row.kind.name.lowercase()} ${row.table}${title?.let { " “$it”" } ?: ""}${if (row.removed) " (table removed)" else ""}", style = MaterialTheme.typography.bodySmall)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button({ model.reauthor(change.id) }) { Text("Re-author") }
                OutlinedButton({ model.dropHeld(change.id) }) { Text("Drop") }
            }
        }
    }
}
