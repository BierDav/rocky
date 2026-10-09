package shopping.qualification

import java.io.File
import java.nio.file.Files
import kotlinx.coroutines.runBlocking
import rocky.*

/** The scope's required definition: lists, categories and items in one `list` group. */
val required = schema("shopping", version = 3) {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id", group = "list") }
    table("categories") {
        integer("list"); text("name"); primaryKey("list", "id"); partitionBy("list", group = "list")
        reference("list", "lists", onDelete = Delete.Cascade)
    }
    table("items") {
        integer("list"); text("key"); text("title"); boolean("completed"); integer("category", nullable = true)
        primaryKey("list", "key"); partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
}

/**
 * The preflight before device qualification: the definition builds by hand and from the shopping app's Room export
 * to the same bytes, and the native runtime installs either and reopens with the other.
 */
fun main() = runBlocking {
    val checkout = generateSequence(File(System.getProperty("user.dir")).absoluteFile) { it.parentFile }.first { File(it, "project.yaml").isFile }
    val export = File(checkout, "examples/shoppinglist-app/shared/schemas/org.example.examples.shopping.ShoppingDatabase/3.json").readText()
    val imported = schemaFromRoom("shopping", export) {
        table("lists") { partitionBy("id", group = "list") }
        table("categories") { partitionBy("list", group = "list") }
        table("items") { partitionBy("list", group = "list"); onConflict("index_items_list_title", Conflict.LatestWins) }
    }
    val definition = required.definitionJson("09".repeat(32))
    check(definition == imported.definitionJson("09".repeat(32))) { "the Room import differs from the hand-written definition" }
    check(required.json() == imported.json()) { "the Room import's installed schema differs" }
    for ((first, second) in listOf(required to imported, imported to required)) {
        val directory = Files.createTempDirectory("rocky-required-schema").toFile()
        try {
            RockyDatabase.local(directory, first).close()
            // Reopening checks the installed descriptor's identity against the other declaration.
            val reopened = RockyDatabase.open(directory, second)
            try { check(Row.decode(reopened.read(second.table("items").query())).isEmpty()) } finally { reopened.close() }
        } finally { directory.deleteRecursively() }
    }
    println(definition)
    println("required shopping schema: hand and Room definitions are identical and install natively")
}
