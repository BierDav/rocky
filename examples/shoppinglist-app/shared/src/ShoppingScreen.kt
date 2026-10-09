package org.example.examples.shopping

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

/** Lists, their categories and items as Room DAO flows on Rocky's file; each action is one repository write. */
@Composable
fun ShoppingScreen(repository: ShoppingRepository, onError: (Throwable) -> Unit = {}) {
    val lists by remember(repository) { repository.lists() }.collectAsState(emptyList())
    var selected by remember { mutableStateOf<Long?>(null) }
    val list = selected?.takeIf { id -> lists.any { it.id == id } } ?: lists.firstOrNull()?.id
    val scope = rememberCoroutineScope()
    var shared by remember { mutableStateOf<Map<String, String>?>(null) }
    fun write(action: suspend () -> Unit) {
        scope.launch { try { action() } catch (error: Exception) { onError(error) } }
    }
    MaterialTheme {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.safeContentPadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Entry("New list", "Create") { title -> write { selected = repository.createList(title) } }
                Entry("Shared list code", "Open") { code -> write { selected = repository.acceptCode(code) } }
                LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(lists, key = { it.id }) { item ->
                        FilterChip(item.id == list, { selected = item.id }, label = { Text(item.title) })
                    }
                }
                if (list != null) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        // One call shares the list with its categories and items.
                        Button(onClick = { write { shared = repository.shareWithDevices(list) } }) { Text("Share") }
                        OutlinedButton(onClick = { write { repository.deleteList(list) } }) { Text("Delete list") }
                    }
                    ListContent(repository, list, ::write)
                }
            }
        }
        shared?.let { codes ->
            AlertDialog(
                onDismissRequest = { shared = null },
                confirmButton = { TextButton(onClick = { shared = null }) { Text("Done") } },
                title = { Text(if (codes.isEmpty()) "No other device is enrolled" else "Give each device its code") },
                text = { SelectionContainer { Column { codes.forEach { (device, code) -> Text("${device.take(8)}: $code") } } } },
            )
        }
    }
}

@Composable
private fun ListContent(repository: ShoppingRepository, list: Long, write: (suspend () -> Unit) -> Unit) {
    val categories by remember(repository, list) { repository.categories(list) }.collectAsState(emptyList())
    val items by remember(repository, list) { repository.observe(list) }.collectAsState(emptyList())
    var category by remember(list) { mutableStateOf<Long?>(null) }
    Entry("New category", "Add") { name -> write { category = repository.addCategory(list, name) } }
    LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        items(categories, key = { it.id }) { item ->
            InputChip(item.id == category, { category = if (category == item.id) null else item.id }, label = { Text(item.name) },
                trailingIcon = { Text("✕", Modifier.clickable { write { repository.deleteCategory(list, item.id) } }) })
        }
    }
    Entry("Add to the list", "Add") { title -> write { repository.add(list, title, category) } }
    val names = categories.associate { it.id to it.name }
    LazyColumn(Modifier.fillMaxWidth()) {
        items(items, key = { it.id }) { item ->
            Row(Modifier.fillMaxWidth().clickable { write { repository.check(list, item.id, !item.completed) } },
                verticalAlignment = Alignment.CenterVertically) {
                Checkbox(item.completed, { write { repository.check(list, item.id, it) } })
                Text(item.title, Modifier.weight(1f))
                // Assigns the selected category, or clears it.
                TextButton(onClick = { write { repository.categorize(list, item.id, category) } }) {
                    Text(item.category?.let(names::get) ?: "No category")
                }
                TextButton(onClick = { write { repository.delete(list, item.id) } }) { Text("Delete") }
            }
        }
    }
}

@Composable
private fun Entry(label: String, action: String, submit: (String) -> Unit) {
    var text by remember { mutableStateOf("") }
    Row(verticalAlignment = Alignment.CenterVertically) {
        OutlinedTextField(text, { text = it }, Modifier.weight(1f), label = { Text(label) }, singleLine = true)
        Spacer(Modifier.width(8.dp))
        Button(onClick = {
            val value = text.trim()
            if (value.isNotEmpty()) { text = ""; submit(value) }
        }) { Text(action) }
    }
}
