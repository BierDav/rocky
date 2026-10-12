@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.shopping

import androidx.room.Room
import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import rocky.PeerTransport
import rocky.SecretStore
import kotlin.test.*
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource

/** Application integration: real Room on Rocky's file through background and reopen. */
class IosShoppingTest {
    @Test fun offlineWriteSurvivesBackgroundAndReopen() = runBlocking<Unit> {
        val began = TimeSource.Monotonic.markNow()
        val root = NSTemporaryDirectory() + "shopping-ios-" + NSUUID().UUIDString
        // A standalone test executable has no app identity for the Keychain, which Rocky's own tests cover.
        val lock = ReentrantLock()
        val stored = mutableMapOf<String, ByteArray>()
        val secrets = object : SecretStore {
            override fun load(name: String) = lock.withLock { stored[name] }
            override fun store(name: String, secret: ByteArray) = lock.withLock { stored[name] = secret }
            override fun delete(name: String) = lock.withLock<Unit> { stored -= name }
        }
        val platform = ShoppingPlatform(
            root,
            "Simulator",
            room = { Room.databaseBuilder<ShoppingDatabase>(name = it) },
            secrets = { secrets },
            transport = { PeerTransport.Iroh.isolated() },
        )
        var node = ShoppingNode.create(platform)
        try {
            node.rocky.setOnline(false)
            val list = node.repository.createList("Offline groceries")
            node.repository.add(list, "Milk")
            node.rocky.enterBackground()
            node.rocky.enterForeground()
            assertEquals("Milk", node.repository.observe(list).first().single().title)
            node.close()
            node = ShoppingNode.resume(platform)
            assertEquals("Offline groceries", node.repository.lists().first().single().title)
            assertEquals("Milk", node.repository.observe(list).first().single().title)
        } finally {
            node.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
        val elapsed = began.elapsedNow()
        println("iOS integration test: $elapsed")
        assertTrue(elapsed < 2.seconds, "Integration budget exceeded: $elapsed")
    }
}
