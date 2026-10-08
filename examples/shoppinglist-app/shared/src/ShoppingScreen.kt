package org.example.examples.shopping

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

/** The list as a Room DAO flow on Rocky's file; each action is one repository write. */
@Composable
fun ShoppingScreen(repository: ShoppingRepository, onError: (Throwable) -> Unit = {}) {
    val items by remember(repository) { repository.observe() }.collectAsState(emptyList())
    val scope = rememberCoroutineScope()
    var title by remember { mutableStateOf("") }
    fun write(action: suspend () -> Unit) {
        scope.launch { try { action() } catch (error: Exception) { onError(error) } }
    }
    MaterialTheme {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.safeContentPadding().padding(16.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(title, { title = it }, Modifier.weight(1f), label = { Text("Add to the list") }, singleLine = true)
                    Spacer(Modifier.width(8.dp))
                    Button(onClick = {
                        val text = title.trim()
                        if (text.isNotEmpty()) { title = ""; write { repository.add(text) } }
                    }) { Text("Add") }
                }
                LazyColumn(Modifier.fillMaxWidth()) {
                    items(items, key = { it.id }) { item ->
                        Row(Modifier.fillMaxWidth().clickable { write { repository.check(item.id, !item.completed) } },
                            verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(item.completed, { write { repository.check(item.id, it) } })
                            Text(item.title, Modifier.weight(1f))
                            TextButton(onClick = { write { repository.delete(item.id) } }) { Text("Delete") }
                        }
                    }
                }
            }
        }
    }
}
