package org.example.examples.shopping.presentation.screen.sync

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.coroutines.delay
import kotlinx.serialization.Serializable
import org.example.examples.shopping.presentation.component.ShowMessages
import org.example.examples.shopping.util.ago
import org.example.examples.shopping.util.epochMillis
import org.example.examples.shopping.util.formatTimestamp

@Serializable
data object SyncKey : NavKey

@Composable
fun SyncScreen(model: SyncViewModel = metroViewModel()) {
    ShowMessages(model.messages)
    val entries by model.entries.collectAsState()
    val filters = model.filters
    val matchingSearch = remember(entries, filters.search) { filterActivity(entries, filters.search) }
    val shown = remember(matchingSearch, filters.selected) { filterActivity(matchingSearch, "", filters.selected) }
    val list = rememberLazyListState()
    val focus = LocalFocusManager.current
    var now by remember { mutableStateOf(epochMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1_000)
            now = epochMillis()
        }
    }
    LaunchedEffect(filters.search, filters.selected) { list.scrollToItem(0) }
    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            value = filters.search,
            onValueChange = { filters.search = it },
            label = { Text("Search activity") },
            placeholder = { Text("Peer, list, error or event ID") },
            leadingIcon = { Icon(Icons.Filled.Search, null) },
            trailingIcon = {
                if (filters.search.isNotEmpty()) IconButton({ filters.search = "" }) { Icon(Icons.Filled.Close, "Clear search") }
            },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { focus.clearFocus() }),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
        )
        LazyRow(contentPadding = PaddingValues(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            item { FilterChip(filters.selected.isEmpty(), filters::all, label = { Text("All (${matchingSearch.size})") }) }
            items(ActivityFilter.entries, key = { it.name }) { filter ->
                val count = matchingSearch.count { filter.matches(it) }
                FilterChip(filter in filters.selected, { filters.toggle(filter) }, label = { Text("${filter.label} ($count)") })
            }
        }
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("${shown.size} of ${entries.size} events · newest first", Modifier.weight(1f), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (filters.search.isNotBlank() || filters.selected.isNotEmpty()) TextButton(filters::clear) { Text("Clear filters") }
        }
        HorizontalDivider()
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            state = list,
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (shown.isEmpty()) {
                item {
                    Text(if (entries.isEmpty()) "No activity yet. Events will appear as Rocky works." else "No events match. Change the search or clear filters.", style = MaterialTheme.typography.bodyMedium)
                }
            }
            items(shown, key = { it.key }) { entry -> EntryRow(entry, now) }
        }
    }
}

@Composable
private fun EntryRow(entry: ActivityEntry, now: Long) {
    val (icon, tint) = entry.kind.look()
    SelectionContainer {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
            Icon(icon, null, Modifier.size(20.dp), tint = tint)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(entry.title, style = MaterialTheme.typography.bodyMedium)
                entry.detail?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                Text("${formatTimestamp(entry.atEpochMillis)} · ${entry.key}", style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Spacer(Modifier.width(8.dp))
            Text(ago(entry.atEpochMillis, now), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
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
