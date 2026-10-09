package org.example.examples.shopping.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.ContributesIntoMap
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.binding
import dev.zacsweers.metrox.viewmodel.ViewModelKey
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.*

/** What the lists screen shows. [selected] is null until a list exists. */
data class ListsState(
    val lists: List<ShoppingList> = emptyList(),
    val selected: ShoppingList? = null,
    val categories: List<Category> = emptyList(),
    val items: List<ShoppingItem> = emptyList(),
    val search: String = "",
    val showCompleted: Boolean = true,
    val category: Long? = null,
    /** Only a trusted device creates, renames, deletes and shares lists. */
    val trusted: Boolean = false,
)

@Inject
@ViewModelKey
@ContributesIntoMap(AppScope::class, binding = binding<ViewModel>())
@OptIn(ExperimentalCoroutinesApi::class)
class ListsViewModel(session: ShoppingSession) : NodeViewModel(session) {
    private val selection = MutableStateFlow<Long?>(null)
    private val filter = MutableStateFlow(Filter())
    private data class Filter(val search: String = "", val showCompleted: Boolean = true, val category: Long? = null)

    val state: StateFlow<ListsState> = node.flatMapLatest { node ->
        val repository = node.repository
        combine(repository.lists(), selection) { lists, selected -> lists to (lists.firstOrNull { it.id == selected } ?: lists.firstOrNull()) }
            .flatMapLatest { (lists, list) ->
                if (list == null) flowOf(ListsState(lists, trusted = node.isTrusted))
                else combine(repository.categories(list.id), filter.flatMapLatest { f ->
                    repository.observe(list.id, f.search, f.showCompleted).map { f to it }
                }) { categories, (f, items) ->
                    ListsState(lists, list, categories, items, f.search, f.showCompleted, f.category?.takeIf { id -> categories.any { it.id == id } }, node.isTrusted)
                }
            }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), ListsState())

    fun select(list: Long) { selection.value = list; filter.update { it.copy(category = null) } }
    fun search(text: String) = filter.update { it.copy(search = text) }
    fun showCompleted(show: Boolean) = filter.update { it.copy(showCompleted = show) }
    /** New items go into the selected category. */
    fun selectCategory(category: Long?) = filter.update { it.copy(category = if (it.category == category) null else category) }

    fun createList(title: String) = write { selection.value = repository.createList(title) }
    fun renameList(title: String) = onList { repository.renameList(it, title) }
    fun deleteList() = onList { repository.deleteList(it) }
    fun addCategory(name: String) = onList { list -> repository.addCategory(list, name).also { id -> filter.update { it.copy(category = id) } } }
    fun deleteCategory(category: Long) = onList { repository.deleteCategory(it, category) }
    fun add(title: String) = onList { repository.add(it, title, state.value.category) }
    fun toggle(item: ShoppingItem) = write { repository.check(item.list, item.id, !item.completed) }
    fun rename(item: ShoppingItem, title: String) = write { repository.rename(item.list, item.id, title) }
    fun categorize(item: ShoppingItem, category: Long?) = write { repository.categorize(item.list, item.id, category) }
    fun delete(item: ShoppingItem) = write { repository.delete(item.list, item.id) }
    fun move(item: ShoppingItem, to: Long) = write { repository.move(item.list, item.id, to) }

    private fun onList(action: suspend ShoppingNode.(Long) -> Unit) {
        val list = state.value.selected?.id ?: return
        write { action(list) }
    }
}
