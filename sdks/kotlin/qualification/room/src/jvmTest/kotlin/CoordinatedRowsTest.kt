package rocky.room.qualification

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import rocky.*
import rocky.room.followRocky
import rocky.room.setRockyDatabase
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Ordinary Room rows follow committed coordinated writes. */
class CoordinatedRowsTest {
    @Test fun restockingCommitsOrdinaryRowsAndInvalidatesRoom() = runBlocking {
        val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "rust-toolchain.toml").isFile }
        val schema = schemaFromRoom("market", File(root, "sdks/kotlin/qualification/room/schemas/rocky.room.qualification.MarketDatabase/1.json").readText())
        val directory = Files.createTempDirectory("room-coordinated").toFile()
        val log = DevelopmentLog()
        val database = RockyDatabase.local(File(directory, "market"), schema)
        var room: MarketDatabase? = null
        val jobs = mutableListOf<Job>()
        try {
            log.configure(database)
            database.coordinate(log)
            val opened = Room.databaseBuilder<MarketDatabase>(File(directory, "room").path).setRockyDatabase(database).build().also { room = it }
            val market = opened.market()
            jobs += launch(Dispatchers.IO) { opened.followRocky(database) }
            val seen = Channel<List<Pair<String, Long>>>(Channel.UNLIMITED)
            jobs += launch(Dispatchers.IO) { market.observeStock().collect { rows -> seen.send(rows.map { it.item to it.remaining }) } }
            suspend fun until(expected: List<Pair<String, Long>>) = withTimeout(20_000) {
                while (seen.receive() != expected) { }
            }
            until(emptyList())
            assertEquals(5L, database.restock("bolt", 5))
            until(listOf("bolt" to 5L))
            assertEquals(7L, database.restock("bolt", 2))
            until(listOf("bolt" to 7L))
            assertEquals(2L, database.coordinatedStatus().position)
            assertEquals(7L, market.stock().single().remaining)
        } finally {
            jobs.forEach { it.cancelAndJoin() }
            room?.close()
            database.close()
            log.close()
            directory.deleteRecursively()
        }
    }
}
