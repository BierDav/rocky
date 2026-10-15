@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import platform.Foundation.NSFileManager
import platform.Foundation.NSNotificationCenter
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.UIKit.UIApplicationDidEnterBackgroundNotification
import platform.UIKit.UIApplicationWillEnterForegroundNotification
import rocky.ios.RockyLifecycle
import kotlin.test.*

class IosLifecycleTest {
    @Test fun notificationsApplyInOrderAndTheLatestVisibilityWinsWithoutClosingDatabase() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-notifications-" + NSUUID().UUIDString
        val definition = schema("notifications", 1) {
            table("items") {
                text("key")
                primaryKey("key")
            }
        }
        val db = RockyDatabase.local(root, definition)
        val center = NSNotificationCenter()
        val lifecycle = RockyLifecycle(false, center)
        val applied = Channel<Boolean>(Channel.UNLIMITED)
        val errors = Channel<Throwable>(Channel.UNLIMITED)
        try {
            lifecycle.follow(db, { applied.trySend(it) }, { errors.trySend(it) })
            withTimeout(1_000) { assertFalse(applied.receive()) }
            center.postNotificationName(UIApplicationWillEnterForegroundNotification, null)
            withTimeout(1_000) { assertTrue(applied.receive()) }
            lifecycle.sceneVisibility(false)
            withTimeout(1_000) { assertFalse(applied.receive()) }
            lifecycle.sceneVisibility(true)
            withTimeout(1_000) { assertTrue(applied.receive()) }
            repeat(10) {
                center.postNotificationName(UIApplicationDidEnterBackgroundNotification, null)
                center.postNotificationName(UIApplicationWillEnterForegroundNotification, null)
            }
            // A final background transition is retained even if intermediate states are conflated.
            center.postNotificationName(UIApplicationDidEnterBackgroundNotification, null)
            withTimeout(1_000) { while (applied.receive()) {} }
            assertFalse(lifecycle.isForeground)
            assertTrue(errors.tryReceive().isFailure)
            lifecycle.unfollow(db)
            lifecycle.unregister()
            db.write(RockyDatabase.newHandle()) {
                val id = nextId("items")
                execute("INSERT INTO items(id,key) VALUES(?,'still open')", id)
            }
        } finally {
            lifecycle.unregister()
            db.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
