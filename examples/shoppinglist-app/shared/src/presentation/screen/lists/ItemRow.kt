package org.example.examples.shopping.presentation.screen.lists

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.DriveFileMove
import androidx.compose.material.icons.automirrored.filled.Label
import androidx.compose.material.icons.automirrored.filled.LabelOff
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import org.example.examples.shopping.data.db.entity.ShoppingItem

@Composable
internal fun ItemRow(
    item: ShoppingItem,
    state: ListsState,
    onToggle: () -> Unit,
    onCategorize: (Long, Boolean) -> Unit,
    onShift: (Int) -> Unit,
    onMove: (Long) -> Unit,
    onRename: (String) -> Unit,
    onEditingFocus: (Boolean) -> Unit,
    onDelete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val categories = state.categoriesOf(item)
    val index = state.items.indexOfFirst { it.id == item.id }
    Card(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(18.dp),
        colors = CardDefaults.cardColors(
            containerColor = if (item.completed) MaterialTheme.colorScheme.surfaceContainerLow else MaterialTheme.colorScheme.surfaceContainer,
        ),
    ) {
        Row(Modifier.padding(start = 4.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Checkbox(item.completed, { onToggle() })
            Column(Modifier.weight(1f).padding(vertical = 10.dp)) {
                ItemTitle(item, onRename, onEditingFocus)
                if (categories.isNotEmpty()) {
                    Text(categories.joinToString(" · ") { it.name }, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                }
            }
            Box {
                IconButton({ menu = true }) { Icon(Icons.Filled.MoreVert, "Actions for ${item.title}") }
                DropdownMenu(menu, { menu = false }) {
                    if (index > 0) {
                        DropdownMenuItem({ Text("Move up") }, {
                            menu = false
                            onShift(-1)
                        }, leadingIcon = { Icon(Icons.Filled.ArrowUpward, null) })
                    }
                    if (index in 0 until state.items.lastIndex) {
                        DropdownMenuItem({ Text("Move down") }, {
                            menu = false
                            onShift(1)
                        }, leadingIcon = { Icon(Icons.Filled.ArrowDownward, null) })
                    }
                    // Categories toggle: an item may be in several.
                    state.categories.forEach { target ->
                        val member = categories.any { it.id == target.id }
                        DropdownMenuItem({ Text(target.name) }, {
                            menu = false
                            onCategorize(target.id, !member)
                        }, leadingIcon = {
                            Icon(if (member) Icons.AutoMirrored.Filled.LabelOff else Icons.AutoMirrored.Filled.Label, null)
                        }, trailingIcon = { if (member) Icon(Icons.Filled.Check, "In ${target.name}") })
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
private fun ItemTitle(item: ShoppingItem, onRename: (String) -> Unit, onEditingFocus: (Boolean) -> Unit) {
    val text = rememberTextFieldState()
    var editing by remember { mutableStateOf(false) }
    var focused by remember { mutableStateOf(false) }
    val requester = remember { FocusRequester() }
    val focus = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    val finish by rememberUpdatedState {
        if (editing) {
            editing = false
            text.text.toString().trim().takeIf { it.isNotEmpty() && it != item.title }?.let(onRename)
        }
    }
    val reportFocus by rememberUpdatedState(onEditingFocus)
    DisposableEffect(Unit) {
        onDispose {
            finish()
            if (focused) reportFocus(false)
        }
    }
    LaunchedEffect(editing) {
        if (editing) requester.requestFocus()
    }
    if (editing) {
        BasicTextField(
            state = text,
            modifier = Modifier.fillMaxWidth().focusRequester(requester).onFocusChanged {
                val lostFocus = focused && !it.isFocused
                focused = it.isFocused
                onEditingFocus(it.isFocused)
                if (lostFocus) finish()
            },
            textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
            lineLimits = TextFieldLineLimits.SingleLine,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            onKeyboardAction = {
                focus.clearFocus(force = true)
                finish()
                keyboard?.hide()
            },
        )
        HorizontalDivider(Modifier.padding(top = 4.dp), color = MaterialTheme.colorScheme.primary)
    } else {
        Text(
            item.title,
            modifier = Modifier.fillMaxWidth().clickable(onClickLabel = "Edit ${item.title}") {
                text.setTextAndPlaceCursorAtEnd(item.title)
                editing = true
            },
            style = MaterialTheme.typography.bodyLarge,
            textDecoration = if (item.completed) TextDecoration.LineThrough else null,
            color = if (item.completed) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        )
    }
}
