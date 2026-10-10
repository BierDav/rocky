package org.example.examples.shopping.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.DriveFileMove
import androidx.compose.material.icons.automirrored.filled.Label
import androidx.compose.material.icons.automirrored.filled.LabelOff
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.ShoppingItem

@Composable
fun ListsScreen(model: ListsViewModel) {
    val state by model.state.collectAsState()
    var naming by remember { mutableStateOf<Naming?>(null) }
    Column(Modifier.fillMaxSize()) {
        ListTabs(state, onSelect = model::select, onNew = { naming = Naming.NewList })
        val list = state.selected
        if (list == null) {
            Empty(
                if (state.trusted) "Create your first list" else "No list was shared with this device yet",
                if (state.trusted) {
                    "Lists sync to every trusted and full device, and to partial devices you share them with."
                } else {
                    "A trusted device shares lists from its Devices tab. This device receives their keys automatically."
                },
            )
            return@Column
        }
        ListHeader(
            state,
            onRename = { naming = Naming.RenameList(list.title) },
            onDelete = model::deleteList,
            onSearch = model::search,
            onShowCompleted = model::showCompleted,
        )
        Categories(state, onSelect = model::selectCategory, onAdd = { naming = Naming.NewCategory }, onDelete = model::deleteCategory)
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items(state.items, key = { it.id }) { item ->
                ItemRow(
                    item,
                    state,
                    onToggle = { model.toggle(item) },
                    onCategorize = { model.categorize(item, it) },
                    onMove = { model.move(item, it) },
                    onRename = { naming = Naming.RenameItem(item) },
                    onDelete = { model.delete(item) },
                )
            }
            if (state.items.isEmpty()) {
                item {
                    Text(
                        if (state.search.isEmpty()) "Nothing on this list yet" else "No item matches",
                        Modifier.padding(24.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        AddItem(state.categories.firstOrNull { it.id == state.category }?.name, model::add)
    }
    naming?.let { current ->
        NameDialog(current.title, current.initial, onDismiss = { naming = null }) { name ->
            naming = null
            when (current) {
                Naming.NewList -> model.createList(name)
                is Naming.RenameList -> model.renameList(name)
                Naming.NewCategory -> model.addCategory(name)
                is Naming.RenameItem -> model.rename(current.item, name)
            }
        }
    }
}

private sealed class Naming(val title: String, val initial: String = "") {
    data object NewList : Naming("New list")
    class RenameList(title: String) : Naming("Rename list", title)
    data object NewCategory : Naming("New category")
    class RenameItem(val item: ShoppingItem) : Naming("Rename item", item.title)
}

@Composable
private fun ListTabs(state: ListsState, onSelect: (Long) -> Unit, onNew: () -> Unit) {
    LazyRow(
        contentPadding = PaddingValues(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        items(state.lists, key = { it.id }) { list ->
            FilterChip(list.id == state.selected?.id, { onSelect(list.id) }, label = { Text(list.title) })
        }
        if (state.trusted) item { AssistChip(onNew, label = { Text("New list") }, leadingIcon = { Icon(Icons.Filled.Add, null) }) }
    }
}

@Composable
private fun ListHeader(state: ListsState, onRename: () -> Unit, onDelete: () -> Unit, onSearch: (String) -> Unit, onShowCompleted: (Boolean) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    val left = state.items.count { !it.completed }
    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(state.selected?.title.orEmpty(), style = MaterialTheme.typography.headlineSmall)
            Text(if (left == 0) "All done" else "$left to buy", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        FilterChip(
            state.showCompleted,
            { onShowCompleted(!state.showCompleted) },
            label = { Text("Done") },
            leadingIcon = { Icon(if (state.showCompleted) Icons.Filled.Visibility else Icons.Filled.VisibilityOff, null, Modifier.size(18.dp)) },
        )
        if (state.trusted) {
            Box {
                IconButton({ menu = true }) { Icon(Icons.Filled.MoreVert, "List actions") }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text("Rename") }, {
                        menu = false
                        onRename()
                    }, leadingIcon = { Icon(Icons.Filled.Edit, null) })
                    DropdownMenuItem({ Text("Delete list") }, {
                        menu = false
                        onDelete()
                    }, leadingIcon = { Icon(Icons.Filled.Delete, null) })
                }
            }
        }
    }
    OutlinedTextField(
        state.search,
        onSearch,
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
        placeholder = { Text("Search") },
        leadingIcon = { Icon(Icons.Filled.Search, null) },
        singleLine = true,
        trailingIcon = { if (state.search.isNotEmpty()) IconButton({ onSearch("") }) { Icon(Icons.Filled.Clear, "Clear") } },
    )
}

@Composable
private fun Categories(state: ListsState, onSelect: (Long?) -> Unit, onAdd: () -> Unit, onDelete: (Long) -> Unit) {
    LazyRow(contentPadding = PaddingValues(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        items(state.categories, key = { it.id }) { category ->
            InputChip(
                category.id == state.category,
                { onSelect(category.id) },
                label = { Text(category.name) },
                trailingIcon = { Icon(Icons.Filled.Close, "Delete ${category.name}", Modifier.size(16.dp).clickable { onDelete(category.id) }) },
            )
        }
        item { AssistChip(onAdd, label = { Text("Category") }, leadingIcon = { Icon(Icons.Filled.Add, null, Modifier.size(16.dp)) }) }
    }
}

@Composable
private fun ItemRow(
    item: ShoppingItem,
    state: ListsState,
    onToggle: () -> Unit,
    onCategorize: (Long?) -> Unit,
    onMove: (Long) -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val category = state.categories.firstOrNull { it.id == item.category }
    ElevatedCard(Modifier.fillMaxWidth().animateContentSize().clickable(onClick = onToggle)) {
        Row(Modifier.padding(start = 4.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Checkbox(item.completed, { onToggle() })
            Column(Modifier.weight(1f).padding(vertical = 10.dp)) {
                Text(
                    item.title,
                    style = MaterialTheme.typography.bodyLarge,
                    textDecoration = if (item.completed) TextDecoration.LineThrough else null,
                    color = if (item.completed) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
                )
                category?.let { Text(it.name, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary) }
            }
            Box {
                IconButton({ menu = true }) { Icon(Icons.Filled.MoreVert, "Item actions") }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem({ Text("Rename") }, {
                        menu = false
                        onRename()
                    }, leadingIcon = { Icon(Icons.Filled.Edit, null) })
                    state.categories.filter { it.id != item.category }.forEach { target ->
                        DropdownMenuItem({ Text("Category: ${target.name}") }, {
                            menu = false
                            onCategorize(target.id)
                        }, leadingIcon = { Icon(Icons.AutoMirrored.Filled.Label, null) })
                    }
                    if (item.category != null) {
                        DropdownMenuItem({ Text("No category") }, {
                            menu = false
                            onCategorize(null)
                        }, leadingIcon = { Icon(Icons.AutoMirrored.Filled.LabelOff, null) })
                    }
                    state.lists.filter { it.id != item.list }.forEach { target ->
                        DropdownMenuItem({ Text("Move to ${target.title}") }, {
                            menu = false
                            onMove(target.id)
                        }, leadingIcon = { Icon(Icons.AutoMirrored.Filled.DriveFileMove, null) })
                    }
                    DropdownMenuItem({ Text("Delete") }, {
                        menu = false
                        onDelete()
                    }, leadingIcon = { Icon(Icons.Filled.Delete, null) })
                }
            }
        }
    }
}

@Composable
private fun AddItem(category: String?, onAdd: (String) -> Unit) {
    var text by remember { mutableStateOf("") }
    val submit = {
        text.trim().takeIf { it.isNotEmpty() }?.let {
            onAdd(it)
            text = ""
        }
        Unit
    }
    Surface(tonalElevation = 3.dp) {
        Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                text,
                { text = it },
                Modifier.weight(1f),
                singleLine = true,
                placeholder = { Text(if (category == null) "Add an item" else "Add to $category") },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { submit() }),
            )
            Spacer(Modifier.width(8.dp))
            FilledIconButton(onClick = submit, enabled = text.isNotBlank()) { Icon(Icons.AutoMirrored.Filled.Send, "Add") }
        }
    }
}

@Composable
internal fun NameDialog(title: String, initial: String, onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    var text by remember { mutableStateOf(initial) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    AlertDialog(
        onDismiss,
        confirmButton = { TextButton({ onConfirm(text.trim()) }, enabled = text.isNotBlank()) { Text("OK") } },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } },
        title = { Text(title) },
        text = {
            OutlinedTextField(
                text,
                { text = it },
                Modifier.focusRequester(focus),
                label = { Text("Name") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { if (text.isNotBlank()) onConfirm(text.trim()) }),
            )
        },
    )
}

@Composable
internal fun Empty(title: String, detail: String) {
    Column(Modifier.fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Icon(Icons.Filled.ShoppingCart, null, Modifier.size(48.dp), tint = MaterialTheme.colorScheme.primary)
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(detail, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
