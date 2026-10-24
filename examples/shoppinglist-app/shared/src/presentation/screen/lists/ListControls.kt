package org.example.examples.shopping.presentation.screen.lists

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun ListTabs(state: ListsState, onSelect: (Long) -> Unit, onNew: () -> Unit) {
    LazyRow(
        contentPadding = PaddingValues(horizontal = 20.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        items(state.lists, key = { it.id }) { list ->
            FilterChip(
                list.id == state.selected?.id,
                { onSelect(list.id) },
                label = { Text(list.title) },
                leadingIcon = { if (list.id == state.selected?.id) Icon(Icons.Filled.ShoppingBasket, null, Modifier.size(18.dp)) },
            )
        }
        if (state.canCreateLists) item { AssistChip(onNew, label = { Text("New list") }, leadingIcon = { Icon(Icons.Filled.Add, null, Modifier.size(18.dp)) }) }
    }
}

@Composable
internal fun ListHeader(state: ListsState, content: ListContent, editor: ListsEditor, onShare: () -> Unit, onRename: () -> Unit, onDelete: () -> Unit) {
    var menu by remember(state.selected?.id) { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(state.selected?.title.orEmpty(), style = MaterialTheme.typography.headlineMedium)
                Text(
                    when {
                        content.total == 0 -> "Ready for your next shop"
                        content.remaining == 0 -> "Everything is in the basket"
                        else -> "${content.remaining} of ${content.total} items to buy"
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Box {
                IconButton({ menu = true }) { Icon(Icons.Filled.MoreVert, "List actions") }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text("Share list") }, {
                        menu = false
                        onShare()
                    }, leadingIcon = { Icon(Icons.Filled.Share, null) })
                    DropdownMenuItem({ Text(if (editor.showCompleted) "Hide completed" else "Show completed") }, {
                        menu = false
                        editor.showCompleted = !editor.showCompleted
                    }, leadingIcon = { Icon(if (editor.showCompleted) Icons.Filled.VisibilityOff else Icons.Filled.Visibility, null) })
                    if (state.trusted) {
                        HorizontalDivider()
                        DropdownMenuItem({ Text("Rename list") }, {
                            menu = false
                            onRename()
                        }, leadingIcon = { Icon(Icons.Filled.Edit, null) })
                        DropdownMenuItem({ Text("Delete list", color = MaterialTheme.colorScheme.error) }, {
                            menu = false
                            onDelete()
                        }, leadingIcon = { Icon(Icons.Filled.Delete, null, tint = MaterialTheme.colorScheme.error) })
                    }
                }
            }
        }
        if (content.total > 0) {
            LinearProgressIndicator(
                progress = { (content.total - content.remaining).toFloat() / content.total },
                modifier = Modifier.fillMaxWidth().height(6.dp),
            )
        }
    }
}

@Composable
internal fun Categories(state: ListsState, selected: Long?, onSelect: (Long?) -> Unit, onAdd: () -> Unit, onDelete: (Long) -> Unit) {
    var menu by remember(state.selected?.id) { mutableStateOf(false) }
    LazyRow(contentPadding = PaddingValues(horizontal = 20.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        if (state.categories.isNotEmpty()) item { FilterChip(selected == null, { onSelect(null) }, label = { Text("All items") }) }
        items(state.categories, key = { it.id }) { category ->
            FilterChip(selected == category.id, { onSelect(category.id) }, label = { Text(category.name) })
        }
        item {
            Box {
                AssistChip({ menu = true }, label = { Text("Categories") }, leadingIcon = { Icon(Icons.Filled.Tune, null, Modifier.size(18.dp)) })
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text("New category") }, {
                        menu = false
                        onAdd()
                    }, leadingIcon = { Icon(Icons.Filled.Add, null) })
                    state.categories.forEach { category ->
                        DropdownMenuItem({ Text("Delete ${category.name}") }, {
                            menu = false
                            onDelete(category.id)
                        }, leadingIcon = { Icon(Icons.Filled.DeleteOutline, null) })
                    }
                }
            }
        }
    }
}

@Composable
internal fun ListEmpty(content: ListContent, onClearFilters: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 40.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Icon(
            if (content.filtered) {
                Icons.Filled.SearchOff
            } else if (content.total == 0) {
                Icons.Filled.ShoppingBasket
            } else {
                Icons.Filled.CheckCircleOutline
            },
            null,
            Modifier.size(40.dp),
            tint = MaterialTheme.colorScheme.primary,
        )
        Text(
            if (content.filtered) {
                "No matching items"
            } else if (content.total == 0) {
                "What do you need?"
            } else {
                "You're all done"
            },
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            if (content.filtered) "Try another search or show all items." else "Add an item below to start your list.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (content.filtered) TextButton(onClearFilters) { Text("Show all items") }
    }
}
