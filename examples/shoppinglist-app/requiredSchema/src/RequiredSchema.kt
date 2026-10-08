package shopping.qualification

import rocky.*

/** Phase 16's required definition. This preflight must succeed before device qualification. */
fun main() {
    val required = schema("shopping", version = 3) {
        table("lists") { text("title"); primaryKey("id"); partitionBy("id") }
        table("items") {
            integer("list"); text("key"); text("title"); boolean("completed")
            primaryKey("list", "key"); partitionBy("list")
            unique("list", "title", onConflict = Conflict.LatestWins)
            reference("list", "lists", onDelete = Delete.Cascade)
        }
    }
    println(required.definitionJson("09".repeat(32)))
}
