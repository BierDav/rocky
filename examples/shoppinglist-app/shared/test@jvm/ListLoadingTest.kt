package org.example.examples.shopping

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.example.examples.shopping.data.db.entity.Category
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.db.entity.ShoppingList
import org.example.examples.shopping.presentation.screen.lists.ListsState
import org.example.examples.shopping.presentation.screen.lists.observeDetails
import kotlin.test.*

@OptIn(ExperimentalCoroutinesApi::class)
class ListLoadingTest {
    @Test fun `a recovering selected list does not hide the other list tabs`() = runTest {
        val lists = listOf(ShoppingList(1, "Recovering"), ShoppingList(2, "Available"))
        val categories = MutableSharedFlow<List<Category>>(replay = 1)
        val item = ShoppingItem(1, "milk", "Milk", false, id = 1)
        val states = mutableListOf<ListsState>()
        backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
            ListsState(lists, lists.first(), canCreateLists = true)
                .observeDetails(categories, flowOf(listOf(item)), flowOf(emptyList()))
                .collect(states::add)
        }
        val waiting = states.single()
        assertFalse(waiting.loading)
        assertTrue(waiting.contentLoading)
        assertEquals(lists, waiting.lists)
        assertTrue(waiting.canCreateLists)
        categories.emit(emptyList())
        val ready = states.last()
        assertFalse(ready.contentLoading)
        assertEquals(listOf(item), ready.items)
        assertEquals(lists, ready.lists)
    }
}
