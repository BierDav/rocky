package rocky.room.qualification

import androidx.room.Room
import androidx.sqlite.SQLiteException
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import rocky.*
import rocky.room.setRockyDatabase
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/**
 * A Room application older than its database, and held changes of a newer one; see `docs/runtime/room-integration.md`
 * and `docs/runtime/held-changes.md`.
 */
class RoomGenerationsTest {
    private val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "rust-toolchain.toml").isFile }
    private val schema = schemaFromRoom(
        "shop",
        File(root, "sdks/kotlin/qualification/room/schemas/rocky.room.qualification.ShopDatabase/1.json").readText(),
    )
    private fun room(directory: File, database: RockyDatabase): ShopDatabase = Room.databaseBuilder<ShopDatabase>(File(directory, "shop").path).setRockyDatabase(database).build()

    /** The next version, without `items.price`. */
    private val newer = schema.copy(
        version = 2,
        tables = schema.tables.map { table ->
            if (table.name == "items") table.copy(columns = table.columns.filter { it.name != "price" }) else table
        },
    )
    private suspend fun titles(database: RockyDatabase): List<String> = database.read(
        buildJsonObject {
            put("sql", "SELECT title FROM items ORDER BY title")
            put("max_rows", 8)
        },
    ).getValue("rows").jsonArray.map { it.jsonArray[0].jsonObject.getValue("value").jsonPrimitive.content }

    @Test fun anUnsentChangeTheNewerVersionRemovesIsHeldAndReauthored() = runBlocking {
        val directory = Files.createTempDirectory("room-held").toFile()
        try {
            val created = RockyDatabase.create(directory, schema, PeerTransport.Iroh.isolated(), automaticSync = false)
            try {
                created.installRoomAuthors()
                val shop = room(directory, created)
                try {
                    shop.shop().createList(ShopList("l1", "Groceries"), listOf(ShopItem("i1", "l1", "Milk", price = 1.5)))
                } finally {
                    shop.close()
                }
            } finally {
                created.close()
            }
            val database = RockyDatabase.open(directory, newer)
            try {
                database.installRoomAuthors()
                // Never sent and writing the removed price: held whole, its rows rolled back.
                val held = database.heldChanges()
                assertEquals(1, held.count)
                val item = held.changes.single().rows.single { it.table == "items" }
                assertEquals(HeldKind.Insert, item.kind)
                val price = item.fields.single { it.name == "price" }
                assertTrue(price.removed && price.written)
                assertEquals(1.5, price.value)
                assertEquals(emptyList(), titles(database))
                // The newer version maps it to its schema.
                database.reauthor(held.changes.single().id) {
                    execute("INSERT INTO lists(key,title,id) VALUES(?,?,?)", "l1", "Groceries", nextId("lists"))
                    execute("INSERT INTO items(key,list,title,completed,quantity,id) VALUES(?,?,?,?,?,?)", "i1", "l1", "Milk", false, 1L, nextId("items"))
                }
                assertEquals(0, database.heldChanges().count)
                assertEquals(listOf("Milk"), titles(database))
            } finally {
                database.close()
            }
        } finally {
            directory.deleteRecursively()
        }
    }

    @Test fun anOlderRoomApplicationKeepsWorkingOnWhatTheDatabaseKept() = runBlocking {
        val directory = Files.createTempDirectory("room-generations").toFile()
        try {
            val created = RockyDatabase.create(directory, schema, PeerTransport.Iroh.isolated(), automaticSync = false)
            try {
                created.installRoomAuthors()
                val shop = room(directory, created)
                try {
                    // No price: an unsent write of the removed column would be held.
                    shop.shop().createList(ShopList("l1", "Groceries"), listOf(ShopItem("i1", "l1", "Milk")))
                } finally {
                    shop.close()
                }
            } finally {
                created.close()
            }
            // The application's next version removes `items.price`; this issuer signs that generation.
            RockyDatabase.open(directory, newer).close()

            // The version 1 application opens the newer database and Room validates the tables it declares.
            val database = RockyDatabase.open(directory, schema)
            try {
                database.installRoomAuthors()
                val shop = room(directory, database)
                try {
                    // A DAO on an unchanged table reads and writes.
                    shop.shop().createEmptyList(ShopList("l2", "Hardware"))
                    assertEquals(listOf("Groceries", "Hardware"), shop.shop().lists().map { it.title })
                    // A DAO on the removed column fails with an ordinary SQLite error.
                    val failed = assertFailsWith<SQLiteException> { shop.shop().prices("l1") }
                    assertContains(failed.message.orEmpty(), "no such column")
                } finally {
                    shop.close()
                }
            } finally {
                database.close()
            }
        } finally {
            directory.deleteRecursively()
        }
    }
}
