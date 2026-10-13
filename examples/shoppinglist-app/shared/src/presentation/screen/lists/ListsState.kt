package org.example.examples.shopping.presentation.screen.lists

import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.runtime.*
import org.example.examples.shopping.data.db.entity.Category
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.db.entity.ShoppingList

/** Committed rows, observed independently of editor changes. */
data class ListsState(
    val lists: List<ShoppingList> = emptyList(),
    val selected: ShoppingList? = null,
    val categories: List<Category> = emptyList(),
    val items: List<ShoppingItem> = emptyList(),
    val trusted: Boolean = false,
) {
    fun content(search: String, showCompleted: Boolean, category: Category?): ListContent {
        val selectedCategory = categories.firstOrNull { it.id == category?.id && it.list == category.list }
        val matches = items.filter {
            it.title.contains(search, ignoreCase = true) &&
                (selectedCategory == null || it.category == selectedCategory.id) &&
                (showCompleted || !it.completed)
        }
        return ListContent(
            items = matches,
            remaining = items.count { !it.completed },
            total = items.size,
            category = selectedCategory,
            filtered = search.isNotEmpty() || selectedCategory != null || !showCompleted,
        )
    }
}

/** Text, selection and IME composition have one synchronous owner, never a DAO round trip. */
@Stable
class ListsEditor {
    val search = TextFieldState()
    val draft = TextFieldState()
    var showCompleted by mutableStateOf(true)
    var category by mutableStateOf<Category?>(null)
    var adding by mutableStateOf(false)
        internal set
}

data class ListContent(
    val items: List<ShoppingItem>,
    val remaining: Int,
    val total: Int,
    val category: Category?,
    val filtered: Boolean,
)
