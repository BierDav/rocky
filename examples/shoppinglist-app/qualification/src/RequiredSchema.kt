package org.example.examples.shopping.qualification

import kotlinx.coroutines.runBlocking
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.db.shoppingRoomOverlay
import rocky.*
import java.io.File
import java.nio.file.Files

/**
 * `required-schema`, the preflight before device qualification: the packaged schema matches the current Room export,
 * and the native runtime installs it and reopens it.
 */
internal fun requiredSchema() = runBlocking {
    val checkout = generateSequence(File(System.getProperty("user.dir")).absoluteFile) { it.parentFile }.first { File(it, "project.yaml").isFile }
    val export = File(checkout, "shared/schemas/org.example.examples.shopping.data.db.ShoppingDatabase/8.json").readText()
    val required = ShoppingSchema
    val imported = schemaFromRoom("shopping", export, shoppingRoomOverlay)
    val definition = required.definitionJson("09".repeat(32))
    check(definition == imported.definitionJson("09".repeat(32))) { "the packaged Room export differs from the current source" }
    check(required.json() == imported.json()) { "the Room import's installed schema differs" }
    for ((first, second) in listOf(required to imported, imported to required)) {
        val directory = Files.createTempDirectory("rocky-required-schema").toFile()
        try {
            RockyDatabase.local(directory, first).close()
            // Reopening checks the installed descriptor's identity against the other declaration.
            val reopened = RockyDatabase.open(directory, second)
            try {
                check(Row.decode(reopened.read(second.table("items").query())).isEmpty())
            } finally {
                reopened.close()
            }
        } finally {
            directory.deleteRecursively()
        }
    }
    println(definition)
    println("required shopping schema: packaged Room definition matches the source and installs natively")
}
