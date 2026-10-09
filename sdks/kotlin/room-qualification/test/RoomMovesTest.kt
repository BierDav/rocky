package rocky.room.qualification

import androidx.room.Room
import java.io.File
import java.nio.file.Files
import kotlin.test.*
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonArray
import rocky.*
import rocky.room.moveRockyRow
import rocky.room.setRockyDatabase

class RoomMovesTest {
    @Test fun moveKeepsIdentityAndRoomReadsCommittedDestination() = runBlocking {
        val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "project.yaml").isFile }
        val definition = schemaFromRoom("roommoves", File(root, "sdks/kotlin/room-qualification/schemas/rocky.room.qualification.MoveDatabase/1.json").readText(),
            { table("move_items") { partitionBy("list") } })
        val directory = Files.createTempDirectory("room-moves").toFile()
        val database = RockyDatabase.local(File(directory, "database"), definition)
        try {
            for (partition in listOf("A", "B")) database.partitions.create("move_items", JsonArray(listOf(text(partition))))
            database.credentials.installPseudonym(database.credentials.assignPseudonym(database.credentials.pseudonymRequest("move_items")))
            var id = 0L
            database.write { id = nextId("move_items", "A"); execute("INSERT INTO move_items(list,key,title,id) VALUES(?,?,?,?)", "A", "milk", "Milk", id) }
            val room = Room.databaseBuilder<MoveDatabase>(File(directory, "room").path).setRockyDatabase(database).build()
            try {
                assertEquals(id, room.moves().items("A").single().id)
                val handle = "88".repeat(32)
                room.moveRockyRow(database, handle, "move_items", id, listOf("A"), listOf("B"))
                assertTrue(room.moves().items("A").isEmpty())
                assertEquals(MoveItem("B", "milk", "Milk", id), room.moves().items("B").single())
                room.moveRockyRow(database, handle, "move_items", id, listOf("A"), listOf("B"))
                assertEquals(1, room.moves().items("B").size)
            } finally { room.close() }
        } finally { database.close(); directory.deleteRecursively() }
    }
}
