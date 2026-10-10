@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package consumer

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSRecursiveLock
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import rocky.*
import rocky.ios.KeychainSecretStore
import rocky.room.ROOM_PROFILE
import rocky.sqlite.RockySQLiteDriver
import kotlin.test.*
import kotlin.time.TimeSource

/** Runs outside the source project with only published coordinates and no native build tool. */
class ConsumerTest {
    @Test fun packagedDatabaseCommitsReopensAndReconciles() = runBlocking<Unit> {
        val started = TimeSource.Monotonic.markNow()
        val directory = NSTemporaryDirectory() + "rocky-package-" + NSUUID().UUIDString
        val definition = schema("iospackage", 1) {
            table("items") {
                text("key")
                text("title")
                primaryKey("key")
            }
        }
        val secrets = TrackedSecrets(KeychainSecretStore("rocky.package." + NSUUID().UUIDString))
        val db = RockyDatabase.local(directory, definition, secrets)
        db.installAuthors()
        val handle = RockyDatabase.newHandle()
        try {
            db.write(handle) {
                val id = nextId("items")
                execute("INSERT INTO items(id,key,title) VALUES(?,'one','packaged')", id)
            }
            assertTrue(db.reconcile(handle).getValue("committed").jsonPrimitive.boolean)
            db.enterBackground()
            db.enterForeground()
        } finally {
            db.close()
        }
        val reopened = RockyDatabase.open(directory, definition, secrets = secrets)
        try {
            assertTrue(reopened.reconcile(handle).getValue("committed").jsonPrimitive.boolean)
            assertEquals("packaged", reopened.read(query("SELECT title FROM items")).getValue("rows").jsonArray[0].jsonArray[0].jsonObject.getValue("value").jsonPrimitive.content)
            // Resolve the optional adapters from the package as well.
            val connection = RockySQLiteDriver(reopened, ROOM_PROFILE).open("ignored")
            try {
                val statement = connection.prepare("SELECT title FROM items")
                try {
                    assertTrue(statement.step())
                    assertEquals("packaged", statement.getText(0))
                } finally {
                    statement.close()
                }
            } finally {
                connection.close()
            }
        } finally {
            reopened.close()
            secrets.purge()
            NSFileManager.defaultManager.removeItemAtPath(directory, null)
            val elapsed = started.elapsedNow()
            println("packaged iOS integration test: $elapsed (budget 2 s)")
            assertTrue(elapsed.inWholeMilliseconds < 2_000, "packaged consumer exceeds the integration budget: $elapsed")
        }
    }
}

private class TrackedSecrets(private val store: SecretStore) : SecretStore {
    private val names = mutableSetOf<String>()
    private val lock = NSRecursiveLock()
    private fun <T> locked(block: () -> T): T {
        lock.lock()
        return try {
            block()
        } finally {
            lock.unlock()
        }
    }
    override fun load(name: String): ByteArray? = locked { store.load(name).also { if (it != null) names.add(name) } }
    override fun store(name: String, secret: ByteArray) = locked {
        store.store(name, secret)
        names.add(name)
        Unit
    }
    override fun delete(name: String) = locked {
        store.delete(name)
        names.remove(name)
        Unit
    }
    fun purge() = locked {
        names.toList().forEach(::delete)
    }
}
