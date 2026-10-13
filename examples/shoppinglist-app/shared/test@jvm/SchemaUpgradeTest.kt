package org.example.examples.shopping

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.example.examples.shopping.data.ShoppingRepository
import org.example.examples.shopping.data.coordination.ApprovalCommands
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.db.trustedTables
import org.example.examples.shopping.data.household.NodeKind
import rocky.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** The shopping schema as version 6 shipped it: one category per item and no order. */
private val ShoppingSchemaV6 = schema("shopping", version = 6) {
    table("lists") {
        text("title")
        primaryKey("id")
        partitionBy("id", group = "list")
    }
    table("categories") {
        integer("list")
        text("name")
        primaryKey("list", "id")
        partitionBy("list", group = "list")
        reference("list", "lists", onDelete = Delete.Cascade)
    }
    table("items") {
        integer("list")
        text("key")
        text("title")
        boolean("completed")
        integer("category", nullable = true)
        primaryKey("list", "key")
        partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
    table("requests") {
        text("key")
        text("title")
        text("note")
        text("requester")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    table("decisions") {
        text("key")
        boolean("approved")
        text("decider")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    trustedTables()
    commands = ApprovalCommands
}

/**
 * A household of version 6 moves to version 7: the trusted device that opens it first runs the migration, and a
 * full member enrolled under version 6 then adopts the cut, edits links it was never granted by name, and loses the
 * item it had not synced. See `docs/runtime/shopping-schema-7.md`.
 */
class SchemaUpgradeTest {
    private fun RockyDatabase.reconciled(): Boolean = runBlocking {
        syncOnce().peers.any { peer -> peer.attempts.any { it.operation == AttemptOperation.Reconcile && it.outcome == AttemptOutcome.Completed } }
    }

    @Test fun `a trusted device migrates version 6 and a member adopts the cut`() = runBlocking {
        val root = Files.createTempDirectory("rocky-shopping-upgrade").toFile()
        val (trusted, member) = File(root, "trusted") to File(root, "member")
        // The trusted device keeps its address across reopening, as on a network; the member reaches it to upgrade.
        val port = java.net.ServerSocket(0).use { it.localPort }
        fun trustedTransport() = PeerTransport.Iroh.isolated(bind = "127.0.0.1:$port")
        var a: RockyDatabase? = RockyDatabase.create(trusted, ShoppingSchemaV6, trustedTransport(), automaticSync = false)
        var b: RockyDatabase? = null
        try {
            b = RockyDatabase.join(member, ShoppingSchemaV6, a!!.pairing(NodeKind.Full.profile).uri, PeerTransport.Iroh.isolated(), automaticSync = false)
            val list = a.partitions.nextId("lists")
            a.write {
                check(nextId("lists") == list)
                execute("INSERT INTO lists(id, title) VALUES(?, ?)", list, "Weekly")
            }
            val (dairy, bakery) = listOf("Dairy", "Bakery").map { name ->
                a.write { nextId("categories", list).also { execute("INSERT INTO categories(list, id, name) VALUES(?, ?, ?)", list, it, name) } }
            }
            for ((title, category) in listOf("Milk" to dairy, "Bread" to bakery, "Eggs" to null)) {
                a.write { execute("INSERT INTO items(list, key, title, completed, category, id) VALUES(?, ?, ?, ?, ?, ?)", list, title, title, false, category, nextId("items", list)) }
            }
            assertTrue(b.reconciled())
            a.list(list).share(listOf(b.session().node))
            assertTrue(b.reconciled())
            assertTrue(b.reconciled())
            // Never reaches the trusted device under version 6.
            b.write { execute("INSERT INTO items(list, key, title, completed, id) VALUES(?, ?, ?, ?, ?)", list, "late", "Late", false, nextId("items", list)) }
            b.close()
            b = null
            a.close()
            a = null

            a = RockyDatabase.resume(trusted, ShoppingSchema, trustedTransport(), automaticSync = false)
            val roomA = openShoppingDatabase(trusted, a)
            try {
                val items = roomA.items().all(list)
                assertEquals(listOf("Milk", "Bread", "Eggs"), items.map { it.title })
                assertTrue(items.all { it.category == null && it.position.isNotEmpty() })
                val links = roomA.itemCategories().observe(list).first().map { link -> items.single { it.id == link.item }.title to link.category }
                assertEquals(setOf("Milk" to dairy, "Bread" to bakery), links.toSet())

                b = RockyDatabase.resume(member, ShoppingSchema, PeerTransport.Iroh.isolated(), automaticSync = false)
                val roomB = openShoppingDatabase(member, b)
                try {
                    b.addPeer(a.contact().uri)
                    assertTrue(b.reconciled())
                    assertEquals(listOf("Milk", "Bread", "Eggs"), roomB.items().all(list).map { it.title })
                    // The member's grants named `items`; `item_categories` follows them.
                    val repository = ShoppingRepository(roomB, b)
                    val eggs = roomB.items().all(list).single { it.title == "Eggs" }
                    repository.categorize(list, eggs.id, dairy, member = true)
                    repository.place(list, roomB.items().all(list), eggs.id, 0)
                    repository.add(list, "Butter", setOf(dairy))
                    assertTrue(b.reconciled())
                } finally {
                    roomB.close()
                }
                assertEquals(listOf("Eggs", "Milk", "Bread", "Butter"), roomA.items().all(list).map { it.title })
                val dairyItems = roomA.itemCategories().observe(list).first().filter { it.category == dairy }.map { link -> roomA.items().all(list).single { it.id == link.item }.title }
                assertEquals(setOf("Milk", "Eggs", "Butter"), dairyItems.toSet())
            } finally {
                roomA.close()
            }
        } finally {
            b?.close()
            a?.close()
            root.deleteRecursively()
        }
    }
}
