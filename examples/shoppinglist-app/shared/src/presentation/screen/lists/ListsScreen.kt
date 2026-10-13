package org.example.examples.shopping.presentation.screen.lists

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.input.clearText
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.unit.dp
import androidx.navigation3.runtime.NavKey
import dev.zacsweers.metrox.viewmodel.metroViewModel
import kotlinx.serialization.Serializable
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.presentation.component.ContactDialog
import org.example.examples.shopping.presentation.component.Empty
import org.example.examples.shopping.presentation.component.NameDialog
import org.example.examples.shopping.presentation.component.ShowMessages

@Serializable
data object ListsKey : NavKey

@Composable
fun ListsScreen(model: ListsViewModel = metroViewModel()) {
    ShowMessages(model.messages)
    val state by model.state.collectAsState()
    val recipients by model.recipients.collectAsState()
    val editor = model.editor
    val content by remember(state, editor) {
        derivedStateOf { state.content(editor.search.text.toString(), editor.showCompleted, editor.category) }
    }
    val focus = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    val dismissKeyboard = {
        focus.clearFocus(force = true)
        keyboard?.hide()
        Unit
    }
    var searchFocused by remember { mutableStateOf(false) }
    var draftFocused by remember { mutableStateOf(false) }
    var focusedItem by remember { mutableStateOf<Pair<Long, Long>?>(null) }
    var sharing by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    var naming by remember { mutableStateOf<Naming?>(null) }
    LaunchedEffect(state.selected?.id) {
        dismissKeyboard()
        sharing = false
        deleting = false
        naming = null
    }
    if (sharing) {
        if (state.trusted) {
            AlertDialog(
                { sharing = false },
                confirmButton = { TextButton({ sharing = false }) { Text("Done") } },
                title = { Text("Share ${state.selected?.title.orEmpty()}") },
                text = {
                    LazyColumn {
                        if (recipients.isEmpty()) item { Text("Add a device in the Devices tab to share this list.") }
                        items(recipients, key = { it.node }) { recipient ->
                            TextButton({
                                model.shareWithDevice(recipient.node)
                                sharing = false
                            }) { Text(recipient.name) }
                        }
                    }
                },
            )
        } else {
            ContactDialog("Scan recipient contact", { sharing = false }) { code, label ->
                sharing = false
                model.shareWithContact(code, label)
            }
        }
    }
    if (deleting) {
        AlertDialog(
            onDismissRequest = { deleting = false },
            title = { Text("Delete ${state.selected?.title.orEmpty()}?") },
            text = { Text("This deletes the list and its items on every device it syncs with.") },
            confirmButton = {
                TextButton({
                    deleting = false
                    model.deleteList()
                }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton({ deleting = false }) { Text("Cancel") } },
        )
    }
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        Column(Modifier.widthIn(max = 760.dp).fillMaxSize()) {
            val list = state.selected
            if (list == null) {
                ListTabs(state, onSelect = {
                    dismissKeyboard()
                    model.select(it)
                }, onNew = { naming = Naming.NewList })
                Empty(
                    if (state.trusted) "Your next shop starts here" else "Waiting for your first list",
                    if (state.trusted) {
                        "Create a list, add what you need, and keep it in sync across your devices."
                    } else {
                        "A trusted device can share a list from the Devices tab. It will appear here automatically."
                    },
                )
            } else {
                LazyColumn(
                    Modifier.weight(1f).fillMaxWidth(),
                    contentPadding = PaddingValues(bottom = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    item(key = "lists") {
                        ListTabs(state, onSelect = {
                            dismissKeyboard()
                            model.select(it)
                        }, onNew = { naming = Naming.NewList })
                    }
                    item(key = "header") {
                        ListHeader(
                            state,
                            content,
                            editor,
                            onShare = {
                                dismissKeyboard()
                                sharing = true
                            },
                            onRename = {
                                dismissKeyboard()
                                naming = Naming.RenameList(list.title)
                            },
                            onDelete = {
                                dismissKeyboard()
                                deleting = true
                            },
                        )
                    }
                    item(key = "search") {
                        SearchField(editor.search, onFocus = { searchFocused = it }, onDone = dismissKeyboard)
                    }
                    item(key = "categories") {
                        Categories(state, content.category?.id, onSelect = model::selectCategory, onAdd = { naming = Naming.NewCategory }, onDelete = model::deleteCategory)
                    }
                    items(content.items, key = { "${it.list}:${it.id}" }) { item ->
                        Box(Modifier.padding(horizontal = 20.dp)) {
                            ShoppingRow(item, state, model, onEditingFocus = { focused ->
                                val identity = item.list to item.id
                                if (focused) {
                                    focusedItem = identity
                                } else if (focusedItem == identity) {
                                    focusedItem = null
                                }
                            })
                        }
                    }
                    if (content.items.isEmpty()) {
                        item {
                            ListEmpty(content, onClearFilters = {
                                editor.search.clearText()
                                editor.category = null
                                editor.showCompleted = true
                                dismissKeyboard()
                            })
                        }
                    }
                }
                AddItem(editor, content.category?.name, searchFocused || draftFocused || focusedItem != null, onFocus = { draftFocused = it }, onAdd = model::add, onDone = dismissKeyboard)
            }
        }
    }
    naming?.let { current ->
        NameDialog(current.title, current.initial, onDismiss = { naming = null }) { name ->
            naming = null
            when (current) {
                Naming.NewList -> model.createList(name)
                is Naming.RenameList -> model.renameList(name)
                Naming.NewCategory -> model.addCategory(name)
            }
        }
    }
}

@Composable
private fun ShoppingRow(item: ShoppingItem, state: ListsState, model: ListsViewModel, onEditingFocus: (Boolean) -> Unit) {
    ItemRow(
        item,
        state,
        onToggle = { model.toggle(item) },
        onCategorize = { model.categorize(item, it) },
        onMove = { model.move(item, it) },
        onRename = { model.rename(item, it) },
        onEditingFocus = onEditingFocus,
        onDelete = { model.delete(item) },
    )
}

private sealed class Naming(val title: String, val initial: String = "") {
    data object NewList : Naming("New list")
    class RenameList(title: String) : Naming("Rename list", title)
    data object NewCategory : Naming("New category")
}
