package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

class DatabaseObservationLifetimeTest {
    @Test
    fun databaseCloseDrainsCollectorsBeforeNativeShutdown() = runBlocking {
        val root = Files.createTempDirectory("rocky-observation-lifetime").toFile()
        val schema = schema("observation_lifetime") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val database = RockyDatabase.create(File(root, "database"), schema, PeerTransport.Iroh.isolated(), automaticSync = false)
        try {
            withTimeout(2_000) {
                val snapshot = CompletableDeferred<Unit>()
                val readiness = CompletableDeferred<Unit>()
                val collectors = listOf(
                    launch { database.observe(query("SELECT id FROM items")).collect { snapshot.complete(Unit) } },
                    launch { database.observeStatus().collect { readiness.complete(Unit) } },
                    launch(start = CoroutineStart.UNDISPATCHED) { database.activity().collect() },
                )
                snapshot.await()
                readiness.await()
                database.close()
                collectors.joinAll()
                assertTrue(collectors.all { it.isCancelled })
                currentCoroutineContext().ensureActive()
            }
        } finally {
            database.close()
            root.deleteRecursively()
        }
    }
}
