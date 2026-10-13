package org.example.examples.shopping.presentation.screen.lists

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp

@Composable
internal fun SearchField(search: TextFieldState, onFocus: (Boolean) -> Unit, onDone: () -> Unit) {
    OutlinedTextField(
        state = search,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 12.dp).onFocusChanged { onFocus(it.isFocused) },
        placeholder = { Text("Search this list") },
        leadingIcon = { Icon(Icons.Filled.Search, null) },
        trailingIcon = {
            if (search.text.isNotEmpty()) IconButton({ search.clearText() }) { Icon(Icons.Filled.Close, "Clear search") }
        },
        lineLimits = TextFieldLineLimits.SingleLine,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        onKeyboardAction = { onDone() },
        shape = RoundedCornerShape(18.dp),
    )
}

@Composable
internal fun AddItem(editor: ListsEditor, category: String?, keyboardFocused: Boolean, onFocus: (Boolean) -> Unit, onAdd: () -> Unit, onDone: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, tonalElevation = 2.dp) {
        Column(Modifier.fillMaxWidth()) {
            if (keyboardFocused) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.End) {
                    TextButton(onDone) {
                        Icon(Icons.Filled.KeyboardHide, null, Modifier.size(18.dp))
                        Spacer(Modifier.width(6.dp))
                        Text("Done")
                    }
                }
            }
            Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    state = editor.draft,
                    modifier = Modifier.weight(1f).onFocusChanged { onFocus(it.isFocused) },
                    lineLimits = TextFieldLineLimits.SingleLine,
                    placeholder = { Text(if (category == null) "Add an item" else "Add to $category") },
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                    onKeyboardAction = {
                        onAdd()
                        onDone()
                    },
                    shape = RoundedCornerShape(18.dp),
                )
                Spacer(Modifier.width(10.dp))
                FilledIconButton(onAdd, enabled = editor.draft.text.isNotBlank() && !editor.adding, modifier = Modifier.size(48.dp)) {
                    Icon(Icons.Filled.Add, "Add item")
                }
            }
        }
    }
}
