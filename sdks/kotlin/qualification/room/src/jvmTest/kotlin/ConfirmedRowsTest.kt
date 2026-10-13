package rocky.room.qualification

import androidx.room.Room
import androidx.room.immediateTransaction
import androidx.room.useWriterConnection
import androidx.sqlite.SQLiteException
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import rocky.*
import rocky.room.followRocky
import rocky.room.setRockyDatabase
import java.io.File
import java.nio.file.Files
import kotlin.test.*

data class Restocking(val item: String, val quantity: Long)

/** Adds to an item's confirmed stock. Every executor derives the same `id` for an item. */
val Restock = Command(
    "Restock",
    handler = "market.restock.v1",
    arguments = Shape(
        listOf(Field("item", FieldType.Text), Field("quantity", FieldType.Integer)),
        { r: Restocking -> listOf(r.item, r.quantity) },
        { Restocking(it[0] as String, it[1] as Long) },
    ),
    accepted = Shape(listOf(Field("remaining", FieldType.Integer)), { n: Long -> listOf(n) }, { it.single() as Long }),
    rejected = Shape.none,
    invocation = "holds",
    result = "holds",
    resources = listOf("holds", "stock"),
)
val restock = Restock handledBy { context, request ->
    // A `stock` row is its `item`, then `remaining`, `id` and `row_kind`.
    val remaining = (context.get("stock", request.item)?.get(1) as Long? ?: 0) + request.quantity
    val id = (1L shl 32) + (request.item.hashCode().toLong() and 0x7fffffff) + 1
    Decision.accept(remaining, listOf(Write.Put("stock", listOf(request.item, remaining, id, 0L))))
}

/** Command-controlled tables through Room: DAOs read confirmed rows, writes fail, confirmation invalidates. */
class ConfirmedRowsTest {
    private val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "rust-toolchain.toml").isFile }
    private val schema = schemaFromRoom(
        "market",
        File(root, "sdks/kotlin/qualification/room/schemas/rocky.room.qualification.MarketDatabase/1.json").readText(),
    ) {
        table("stock") { tableClass = TableClass.CommandControlled }
        commands = CommandSet(listOf(Restock))
    }
    private fun loopback(): PeerTransport = PeerTransport.Iroh.isolated()
    private fun bundles(directory: File): Long {
        // Keep another SQLite library out of the process while Rocky holds the WAL.
        val process = ProcessBuilder("sqlite3", "-readonly", File(directory, "rocky.sqlite").path, "SELECT count(DISTINCT handle) FROM secure_bundle_handles").start()
        check(process.waitFor(10, java.util.concurrent.TimeUnit.SECONDS)) {
            process.destroyForcibly().waitFor()
            "SQLite inspection timed out"
        }
        val output = process.inputStream.bufferedReader().readText()
        val error = process.errorStream.bufferedReader().readText()
        check(process.exitValue() == 0) { error }
        return output.trim().toLong()
    }

    @Test fun daosReadConfirmedRowsAndTheirWritesFail() = runBlocking {
        val directory = Files.createTempDirectory("room-confirmed").toFile()
        val coordinator = DevelopmentCoordinator()
        val data = File(directory, "market")
        val database = RockyDatabase.create(
            data,
            schema,
            loopback(),
            automaticSync = false,
            commands = CommandRuntime(coordinator, listOf(restock)),
        )
        val work = mutableListOf<Job>()
        try {
            coordinator.register(database.commandIdentity())
            database.installRoomAuthors(tables = listOf("holds"))
            val room = Room.databaseBuilder<MarketDatabase>(File(directory, "market.room").path).setRockyDatabase(database).build()
            val market = room.market()
            work += launch(Dispatchers.IO) { room.followRocky(database) }
            val seen = Channel<List<Pair<String, Long>>>(Channel.UNLIMITED)
            work += launch(Dispatchers.IO) { market.observeStock().collect { rows -> seen.send(rows.map { it.item to it.remaining }) } }
            suspend fun until(expected: List<Pair<String, Long>>) = withTimeout(20_000) {
                while (true) if (seen.receive() == expected) break
            }
            until(emptyList())

            // A queued command is not a row; its confirmed result is, and it invalidates the DAO's Flow.
            val first = RockyDatabase.newHandle()
            assertEquals(TicketState.Queued, database.submit(Restock, Restocking("bolt", 5), first).state)
            assertEquals(emptyList(), market.stock())
            assertEquals(TicketState.Applied, database.run(Restock, first).state)
            until(listOf("bolt" to 5L))
            val confirmed = market.stock().single()
            assertEquals(listOf<Any?>("bolt", 5L, confirmed.id, 0L), database.confirmedRows("stock").rows.single())

            // Every DAO write kind fails with SQLITE_AUTH, and a transaction with one publishes nothing.
            val published = bundles(data)
            for (write in listOf<suspend () -> Unit>(
                { market.insertStock(Stock("nut", 1, id = (1L shl 32) + 9)) },
                { market.updateStock(confirmed.copy(remaining = 0)) },
                { market.clearStock() },
                { market.holdAndSetStock(Hold("h1", "bolt"), confirmed.copy(remaining = 4)) },
            )) {
                val refused = assertFailsWith<SQLiteException> { write() }
                assertContains(refused.message.orEmpty(), "Error code: 23")
            }
            assertEquals(published, bundles(data))
            assertEquals(emptyList(), market.holds())
            assertEquals(listOf(confirmed), market.stock())

            // The mergeable table beside it stays writable.
            room.useWriterConnection { writer ->
                writer.immediateTransaction { market.insertHold(Hold("h2", "bolt", id = market.nextId("holds"))) }
            }
            assertEquals(listOf("h2"), market.holds().map { it.key })
            assertEquals(published + 1, bundles(data))

            val second = RockyDatabase.newHandle()
            database.submit(Restock, Restocking("bolt", 2), second)
            assertEquals(TicketState.Applied, database.run(Restock, second).state)
            until(listOf("bolt" to 7L))
            work.forEach { it.cancelAndJoin() }
            work.clear()
            room.close()
        } finally {
            work.forEach { it.cancelAndJoin() }
            database.close()
            directory.deleteRecursively()
        }
    }
}
