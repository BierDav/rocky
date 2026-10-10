@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.shopping

import androidx.room.Room
import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import rocky.PeerTransport
import rocky.SecretStore
import rocky.ios.KeychainSecretStore
import kotlin.test.*
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource

/** Application integration: real Room on Rocky's file and platform Keychain secrets. */
class IosShoppingTest {
    @Test fun offlineWriteSurvivesBackgroundAndReopen() = runBlocking<Unit> {
        val began = TimeSource.Monotonic.markNow()
        val root = NSTemporaryDirectory() + "shopping-ios-" + NSUUID().UUIDString
        val keychain = KeychainSecretStore("org.example.shopping.test." + NSUUID().UUIDString)
        val lock = ReentrantLock()
        val names = mutableSetOf<String>()
        val secrets = object : SecretStore by keychain {
            override fun store(name: String, secret: ByteArray) = lock.withLock {
                names += name
                keychain.store(name, secret)
            }
        }
        val platform = ShoppingPlatform(
            root,
            "Simulator",
            room = { Room.databaseBuilder<ShoppingDatabase>(name = it) },
            secrets = secrets,
            transport = { PeerTransport.Tcp("127.0.0.1:0") },
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
            lock.withLock { names.forEach(keychain::delete) }
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
        val elapsed = began.elapsedNow()
        println("iOS integration test: $elapsed")
        assertTrue(elapsed < 2.seconds, "Integration budget exceeded: $elapsed")
    }
}
