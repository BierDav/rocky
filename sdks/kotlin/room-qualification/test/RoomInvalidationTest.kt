package rocky.room.qualification

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import rocky.*
import rocky.room.followRocky
import rocky.room.setRockyDatabase
import java.io.File
import java.net.ServerSocket
import java.nio.file.Files
import kotlin.test.*

/** DAO observers follow every commit of Rocky's tables, sync writes included, with no trigger on Room's side. */
class RoomInvalidationTest {
    private val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "project.yaml").isFile }
    private val schema = schemaFromRoom(
        "shop",
        File(root, "sdks/kotlin/room-qualification/schemas/rocky.room.qualification.ShopDatabase/1.json").readText(),
    )
    private fun tcp(): PeerTransport = PeerTransport.Tcp(ServerSocket(0).use { "127.0.0.1:${it.localPort}" })
    private fun item(key: String, title: String) = ShopItem(key, "l1", title)

    @Test fun daoObserversFireOnLocalAndSyncWrites() = runBlocking {
        val directory = Files.createTempDirectory("room-invalidation").toFile()
        val a = RockyDatabase.create(File(directory, "a"), schema, tcp(), automaticSync = false)
        var b: RockyDatabase? = null
        val work = mutableListOf<Job>()
        try {
            a.installAuthors()
            b = RockyDatabase.join(
                File(directory, "b"),
                schema,
                a.pairing(GrantProfile.Writer(setOf("lists", "items"))).uri,
                tcp(),
                automaticSync = false,
            )
            b.installAuthors(issuer = a)
            val roomA = Room.databaseBuilder<ShopDatabase>(File(directory, "a.room").path).setRockyDatabase(a).build()
            val roomB = Room.databaseBuilder<ShopDatabase>(File(directory, "b.room").path).setRockyDatabase(b).build()
            work += launch(Dispatchers.IO) { roomB.followRocky(b) }
            val seen = Channel<List<String>>(Channel.UNLIMITED)
            work += launch(Dispatchers.IO) { roomB.shop().observe("l1").collect { items -> seen.send(items.map { it.title }) } }
            suspend fun until(expected: List<String>) = withTimeout(20_000) {
                while (true) if (seen.receive() == expected) break
            }
            until(emptyList())
            // A local DAO write: Room's own refresh after its transaction.
            roomB.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk")))
            until(listOf("Milk"))
            // Another device's write arrives by sync, never through Room's connection.
            val eggs = roomA.shop().createItems(listOf(item("i2", "Eggs"))).single()
            assertEquals(ConnectionState.Connected, b.syncOnce().state)
            until(listOf("Eggs", "Milk"))
            roomA.shop().update(eggs.copy(title = "Free-range eggs"))
            b.syncOnce()
            until(listOf("Free-range eggs", "Milk"))
            assertEquals(listOf("Free-range eggs", "Milk"), roomB.shop().items("l1").map { it.title })
            work.forEach { it.cancelAndJoin() }
            work.clear()
            roomA.close()
            roomB.close()
        } finally {
            work.forEach { it.cancelAndJoin() }
            b?.close()
            a.close()
            directory.deleteRecursively()
        }
    }
}
