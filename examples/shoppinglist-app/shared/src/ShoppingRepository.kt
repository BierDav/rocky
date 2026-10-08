package org.example.examples.shopping

import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.json.JsonPrimitive
import rocky.*

/**
 * [ShoppingDatabase]'s tables as Rocky replicates them; importing its Room export yields the same definition. `id`
 * and `row_kind` are implicit. A device inserts items under its own author for `shopping_items`, which an issuer
 * assigns it.
 */
val ShoppingSchema = schema("shopping", version = 2) {
    table("shopping_items") {
        text("title"); boolean("completed")
        column(Column.text("searchTitle").copy(default = JsonPrimitive("")))
        primaryKey("id"); index("index_shopping_items_completed_title", "completed", "title")
    }
}

/** Plain DAO reads and writes on Rocky's file: each write is one commit and one signed bundle. */
class ShoppingRepository(private val database: ShoppingDatabase) {
    fun observe(search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>> =
        database.items().observe(search.lowercase(), includeCompleted)
    /** Returns the id Rocky issued; the device needs its author for `shopping_items`, see [ShoppingSchema]. */
    suspend fun add(title: String): Long {
        require(title.isNotBlank())
        return database.items().add(title).id
    }
    suspend fun rename(id: Long, title: String) {
        require(title.isNotBlank())
        database.items().rename(id, title, title.lowercase())
    }
    suspend fun check(id: Long, completed: Boolean) { database.items().check(id, completed) }
    suspend fun delete(id: Long) { database.items().delete(id) }
}
