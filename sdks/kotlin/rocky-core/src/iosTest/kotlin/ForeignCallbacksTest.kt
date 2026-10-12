@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.posix.pthread_self
import rocky.internal.native.bindingLiveHandles
import kotlin.test.*

class ForeignCallbacksTest {
    @Test fun publicSecretStoreEnrollsThroughRust() = runBlocking {
        withTimeout(30_000) {
            val caller = pthread_self()
            val lock = ReentrantLock()
            val keys = mutableMapOf<String, ByteArray>()
            val calls = atomic(0)
            val secrets = object : SecretStore {
                private fun called() {
                    if (pthread_self() != caller) calls.incrementAndGet()
                }
                override fun load(name: String): ByteArray? = lock.withLock {
                    called()
                    keys[name]?.copyOf()
                }
                override fun store(name: String, secret: ByteArray) = lock.withLock {
                    called()
                    keys[name] = secret.copyOf()
                }
                override fun delete(name: String) {
                    lock.withLock {
                        called()
                        keys.remove(name)
                    }
                }
            }
            val root = NSTemporaryDirectory() + "rocky-foreign-" + NSUUID().UUIDString
            val definition = schema("foreigncallbacks", 1) {
                table("items") {
                    text("key")
                    primaryKey("key")
                }
            }
            val owner = RockyDatabase.create("$root/owner", definition, PeerTransport.Iroh.isolated(), secrets, automaticSync = false)
            var member: RockyDatabase? = null
            try {
                val invitation = owner.pairing(GrantProfile.Writer(setOf("items")))
                member = RockyDatabase.join("$root/member", definition, invitation.uri, PeerTransport.Iroh.isolated(), automaticSync = false)
                assertEquals(owner.domain, member.domain)
                assertTrue(calls.value > 0)
            } finally {
                member?.close()
                owner.close()
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
            while (bindingLiveHandles() != 0) yield()
            assertEquals(0, bindingLiveHandles())
        }
    }
}
