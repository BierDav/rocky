package org.example.examples.shopping

import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import org.example.examples.shopping.data.db.entity.Category
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.db.entity.ShoppingList
import org.example.examples.shopping.presentation.screen.lists.ListsEditor
import org.example.examples.shopping.presentation.screen.lists.ListsState
import kotlin.test.*

class ListsStateTest {
    private val dairy = Category(1, 7, "Dairy")
    private val milk = ShoppingItem(1, "milk", "Milk", false, dairy.id, 1)
    private val bread = ShoppingItem(1, "bread", "Bread", false, null, 2)
    private val cheese = ShoppingItem(1, "cheese", "Cheese", true, dairy.id, 3)
    private val state = ListsState(selected = ShoppingList(1, "Weekly"), categories = listOf(dairy), items = listOf(milk, bread, cheese))

    @Test fun `filters intersect while progress describes the whole list`() {
        val content = state.content("MIL", false, dairy)
        assertEquals(listOf(milk), content.items)
        assertEquals(2, content.remaining)
        assertEquals(3, content.total)
        assertEquals(dairy, content.category)
        assertTrue(content.filtered)
    }

    @Test fun `deleted category falls back to all items and cannot cross lists`() {
        val removed = state.copy(categories = emptyList()).content("", true, dairy)
        assertNull(removed.category)
        assertEquals(listOf(milk, bread, cheese), removed.items)
        val otherList = state.copy(categories = listOf(Category(2, dairy.id, "Produce")))
        assertNull(otherList.content("", true, dairy).category)
    }

    @Test fun `checking an item preserves its position and hiding completed preserves relative order`() {
        val mixed = state.copy(items = listOf(cheese, milk, bread))
        assertEquals(listOf(cheese, milk, bread), mixed.content("", true, null).items)
        val checked = mixed.copy(items = listOf(cheese, milk.copy(completed = true), bread))
        assertEquals(listOf(cheese.id, milk.id, bread.id), checked.content("", true, null).items.map { it.id })
        assertEquals(listOf(bread), checked.content("", false, null).items)
        assertEquals(listOf(cheese, milk), mixed.content("", true, dairy).items)
    }

    @Test fun `text edits and clearing filter immediately without new repository rows`() {
        val editor = ListsEditor()
        editor.search.setTextAndPlaceCursorAtEnd("milk")
        assertEquals(listOf(milk), state.content(editor.search.text.toString(), editor.showCompleted, editor.category).items)
        editor.search.edit {
            replace(0, length, "bread")
            selection = androidx.compose.ui.text.TextRange(2)
        }
        assertEquals(listOf(bread), state.content(editor.search.text.toString(), editor.showCompleted, editor.category).items)
        assertEquals(2, editor.search.selection.start)
        state.copy(items = listOf(milk)).content(editor.search.text.toString(), true, null)
        assertEquals("bread", editor.search.text.toString())
        assertEquals(2, editor.search.selection.start)
        editor.search.setTextAndPlaceCursorAtEnd("")
        assertEquals(listOf(milk, bread, cheese), state.content(editor.search.text.toString(), true, null).items)
    }
}
