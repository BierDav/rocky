@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.cinterop.toKString
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.posix.getenv
import kotlin.test.*

/** Also runs in subprocess modes under qualify-ios-lifecycle; secrets use the file fallback under [root]. */
class IosCrashProbeTest {
    private fun environment(name: String): String = getenv(name)?.toKString() ?: error("$name is required")
    private val root = getenv("ROCKY_CRASH_ROOT")?.toKString() ?: (NSTemporaryDirectory() + "rocky-crash-" + NSUUID().UUIDString)
    private val definition = schema("ioscrash", 1) {
        table("items") {
            text("key")
            text("title")
            primaryKey("key")
        }
    }
    private val handle = "18".repeat(32)

    @Test fun committedWriteReopensAndReconciles() {
        when (getenv("ROCKY_CRASH_MODE")?.toKString()) {
            "prepare" -> prepare()

            "write" -> write(expectKill = true)

            "verify" -> verifyAndPurge(environment("ROCKY_CRASH_EXPECT_COMMIT") == "yes")

            null -> {
                prepare()
                write(expectKill = false)
                verifyAndPurge(committed = true)
            }

            else -> error("unknown crash probe mode")
        }
    }

    private fun prepare() = runBlocking<Unit> {
        val db = RockyDatabase.local(root, definition)
        db.close()
    }

    private fun write(expectKill: Boolean): Unit = runBlocking<Unit> {
        val db = RockyDatabase.open(root, definition)
        db.write(handle) {
            val id = nextId("items")
            execute("INSERT INTO items(id,key,title) VALUES(?, 'one','durable')", id)
        }
        if (expectKill) error("the qualification process was not killed at the selected commit point")
        db.close()
    }

    private fun verifyAndPurge(committed: Boolean) = runBlocking<Unit> {
        val db = RockyDatabase.open(root, definition)
        try {
            assertEquals(committed, db.reconcile(handle).getValue("committed").jsonPrimitive.boolean)
            val rows = db.read(query("SELECT title FROM items")).getValue("rows").jsonArray
            assertEquals(if (committed) 1 else 0, rows.size)
            if (committed) assertEquals("durable", rows[0].jsonArray[0].jsonObject.getValue("value").jsonPrimitive.content)
        } finally {
            db.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
