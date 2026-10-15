package rocky.room.qualification

import androidx.room.Room
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonArray
import rocky.*
import rocky.room.moveRockyRow
import rocky.room.setRockyDatabase
import rocky.testing.testRepositoryRoot
import kotlin.test.*
import rocky.testing.TestFile as File
import rocky.testing.TestFiles as Files

class RoomMovesTest {
    @Test fun moveKeepsIdentityAndRoomReadsCommittedDestination() = runBlocking {
        val root = File(testRepositoryRoot())
        val definition = schemaFromRoom(
            "roommoves",
            File(root, "sdks/kotlin/qualification/room/schemas/rocky.room.qualification.MoveDatabase/1.json").readText(),
            { table("move_items") { partitionBy("list") } },
        )
        val directory = Files.createTempDirectory("room-moves").toFile()
        val database = RockyDatabase.local(File(directory, "database").path, definition)
        try {
            for (partition in listOf("A", "B")) database.partitions.create("move_items", JsonArray(listOf(text(partition))))
            var id = 0L
            database.write {
                id = nextId("move_items", "A")
                execute("INSERT INTO move_items(list,key,title,id) VALUES(?,?,?,?)", "A", "milk", "Milk", id)
            }
            val room = Room.databaseBuilder<MoveDatabase>(File(directory, "room").path).setRockyDatabase(database).build()
            try {
                assertEquals(id, room.moves().items("A").single().id)
                val handle = "88".repeat(32)
                room.moveRockyRow(database, handle, "move_items", id, listOf("A"), listOf("B"))
                assertTrue(room.moves().items("A").isEmpty())
                assertEquals(MoveItem("B", "milk", "Milk", id), room.moves().items("B").single())
                room.moveRockyRow(database, handle, "move_items", id, listOf("A"), listOf("B"))
                assertEquals(1, room.moves().items("B").size)
            } finally {
                room.close()
            }
        } finally {
            database.close()
            directory.deleteRecursively()
        }
    }
}
