package org.example.examples.market

import androidx.room.Room
import androidx.sqlite.SQLiteException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.jupiter.api.BeforeAll
import rocky.*
import rocky.room.buildOnRocky
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Loads the native library once per test class, so no test pays for it. */
internal fun loadNative() {
    DevelopmentCoordinator()
}

/** Secrets in memory, for tests. */
internal class MemorySecrets : SecretStore {
    private val secrets = mutableMapOf<String, ByteArray>()
    override fun load(name: String): ByteArray? = synchronized(secrets) { secrets[name] }
    override fun store(name: String, secret: ByteArray) = synchronized(secrets) { secrets[name] = secret }
    override fun delete(name: String) = synchronized(secrets) { secrets.remove(name) }.let { }
}

/** The app's Room DAOs on Rocky's file: listings and memos merge, stock and holds follow confirmed commands only. */
class MarketRoomTest {
    companion object {
        @JvmStatic @BeforeAll
        fun load() = loadNative()
    }

    private val checkout = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "project.yaml").isFile }

    @Test fun `the hand-declared schema is the Room export's`() {
        val export = File(checkout, "examples/market-app/marketShared/schemas/org.example.examples.market.MarketDatabase/1.json").readText()
        val imported = schemaFromRoom("market", export, marketRoomOverlay)
        assertEquals(MarketSchema.json(), imported.json())
        assertEquals(MarketSchema.definitionJson("09".repeat(32)), imported.definitionJson("09".repeat(32)))
    }

    @Test fun `DAOs read confirmed stock and every write to it is refused`() = runBlocking {
        val root = Files.createTempDirectory("market-room-test").toFile()
        val coordinator = DevelopmentCoordinator()
        val rocky = RockyDatabase.create(
            File(root, "seller"),
            MarketSchema,
            PeerTransport.Tcp("127.0.0.1:0"),
            automaticSync = false,
            commands = CommandRuntime(coordinator, MarketHandlers),
        )
        val room = Room.databaseBuilder<MarketDatabase>(File(root, "market").absolutePath).buildOnRocky(rocky)
        try {
            coordinator.register(rocky.commandIdentity())
            val market = Market(rocky, room.room, MemorySecrets()) { ByteArray(it) }
            market.addListing("lamp", "Lamp")
            assertEquals(listOf(Offer("lamp", "Lamp", null)), market.offers().first())
            assertEquals(TicketState.Applied, market.restock("lamp", 2).state)
            withTimeout(5_000) { market.offers().first { it == listOf(Offer("lamp", "Lamp", 2)) } }

            val ticket = market.reserve("lamp", 1, hold = "h1")
            assertEquals(Outcome.Accepted(Remaining(1)), ticket.outcome)
            assertEquals(ticket, market.reservation(ticket.handle)?.copy(error = ticket.error))
            withTimeout(5_000) { market.holds().first { holds -> holds.map { it.key to it.quantity } == listOf("h1" to 1L) } }

            // A DAO write and an SQL write to the command-controlled table both fail and change nothing.
            val stock = room.room.stock().observe().first().single()
            val refused = assertFailsWith<SQLiteException> { room.room.stock().overwrite(stock.copy(remaining = 99)) }
            assertContains(refused.message.orEmpty(), "Error code: 23")
            assertFailsWith<RockyException> { rocky.write { execute("UPDATE stock SET remaining = 99") } }
            assertEquals(listOf(stock), room.room.stock().observe().first())

            // The mergeable tables beside them stay writable; published identities are not memos.
            market.addMemo("lamp", "Blue, please")
            val identity = market.publishIdentity()
            assertEquals(listOf("Blue, please"), market.memos().first().map { it.note })
            assertEquals(listOf(identity.encoded), market.published().map { it.encoded })
            assertFalse(market.isOwner)
        } finally {
            room.close()
            rocky.close()
            root.deleteRecursively()
        }
    }
}
