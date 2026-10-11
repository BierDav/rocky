package org.example.examples.shopping

import androidx.room.useReaderConnection
import kotlinx.coroutines.runBlocking
import org.example.examples.shopping.data.ShoppingRepository
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.data.household.NodeKind
import rocky.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Application privacy and sharing over contacts, with discovery disabled (TCP). */
class RosterFreeTest {
    // These regressions exercise SQLite and TCP; file-key fsync has separate coverage.
    private fun keys(): SecretStore {
        val values = java.util.concurrent.ConcurrentHashMap<String, ByteArray>()
        return object : SecretStore {
            override fun load(name: String) = values[name]?.copyOf()
            override fun store(name: String, secret: ByteArray) {
                values[name] = secret.copyOf()
            }
            override fun delete(name: String) {
                values.remove(name)
            }
        }
    }

    @Test fun `members have no roster and share only after scanning a recipient contact`() = runBlocking {
        val root = Files.createTempDirectory("rocky-roster-free").toFile()
        val nodes = mutableListOf<RockyDatabase>()
        val rooms = mutableListOf<ShoppingDatabase>()
        fun transport() = PeerTransport.Tcp("127.0.0.1:0")
        try {
            val issuer = RockyDatabase.create(File(root, "issuer"), ShoppingSchema, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            val left = RockyDatabase.join(File(root, "left"), ShoppingSchema, issuer.pairing(NodeKind.Full.profile).uri, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            val right = RockyDatabase.join(File(root, "right"), ShoppingSchema, issuer.pairing(NodeKind.Partial.profile).uri, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            val issuerRoom = openShoppingDatabase(File(root, "issuer"), issuer).also(rooms::add)
            val leftRoom = openShoppingDatabase(File(root, "left"), left).also(rooms::add)
            val rightRoom = openShoppingDatabase(File(root, "right"), right).also(rooms::add)
            val household = Household(leftRoom, left, left.session().node)
            assertFails { household.devices() }
            assertFails { household.registry() }
            val repository = ShoppingRepository(issuerRoom, issuer)
            val list = repository.createList("Weekly")
            repository.add(list, "Milk")
            repository.share(list, listOf(left.session().node))
            assertEquals(ConnectionState.Connected, left.syncOnce().state)
            assertEquals(listOf("Milk"), leftRoom.items().all(list).map { it.title })
            right.syncOnce()
            assertFalse(right.list(list).status().isKeyed)
            assertEquals(right.session().node, household.shareWithContact(list, right.contact().uri))
            assertEquals(ConnectionState.Connected, left.syncOnce().state, left.peers().toString())
            assertEquals(ConnectionState.Connected, right.syncOnce().state)
            assertEquals(listOf("Milk"), rightRoom.items().all(list).map { it.title })
            issuer.setOnline(false)
            assertEquals(ConnectionState.Connected, left.syncOnce().state, left.peers().toString())
            assertEquals(listOf("Milk"), rightRoom.items().all(list).map { it.title })
        } finally {
            rooms.asReversed().forEach { it.close() }
            nodes.asReversed().forEach { it.close() }
            root.deleteRecursively()
        }
    }

    @Test fun `member SQL opens no roster tables`() = runBlocking {
        val root = Files.createTempDirectory("rocky-member-tables").toFile()
        val issuer = RockyDatabase.create(File(root, "issuer"), ShoppingSchema, PeerTransport.Tcp("127.0.0.1:0"), secrets = keys(), automaticSync = false)
        var member: RockyDatabase? = null
        var room: ShoppingDatabase? = null
        try {
            member = RockyDatabase.join(File(root, "member"), ShoppingSchema, issuer.pairing(NodeKind.Full.profile).uri, PeerTransport.Tcp("127.0.0.1:0"), secrets = keys(), automaticSync = false)
            val memberRoom = openShoppingDatabase(File(root, "member"), member)
            room = memberRoom
            for (table in listOf("devices", "keys", "identities")) {
                val error = assertFails {
                    memberRoom.useReaderConnection { reader -> reader.usePrepared("SELECT * FROM $table") { it.step() } }
                }
                assertContains(error.message.orEmpty(), "no such table")
            }
        } finally {
            room?.close()
            member?.close()
            issuer.close()
            root.deleteRecursively()
        }
    }

    @Test fun `one contact hands over a second partner without roster or discovery`() = runBlocking {
        val root = Files.createTempDirectory("rocky-contact-handover").toFile()
        val nodes = mutableListOf<RockyDatabase>()
        fun transport() = PeerTransport.Tcp("127.0.0.1:0")
        try {
            val issuer = RockyDatabase.create(File(root, "issuer"), ShoppingSchema, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            val first = RockyDatabase.join(File(root, "first"), ShoppingSchema, issuer.pairing(NodeKind.Full.profile).uri, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            issuer.addPeer(first.contact().uri)
            issuer.syncOnce()
            first.syncOnce()
            val second = RockyDatabase.join(File(root, "second"), ShoppingSchema, issuer.pairing(NodeKind.Full.profile).uri, transport(), secrets = keys(), automaticSync = false).also(nodes::add)
            // Enrollment supplies just the issuer. The first authenticated pass hands over its other contact.
            second.syncOnce()
            assertTrue(second.peers().any { it.node == first.session().node && it.source == "handover" })
            issuer.setOnline(false)
            assertEquals(ConnectionState.Connected, second.syncOnce().state)
            assertTrue(second.peers().any { it.node == first.session().node && it.state == "connected" })
        } finally {
            nodes.asReversed().forEach { it.close() }
            root.deleteRecursively()
        }
    }
}
