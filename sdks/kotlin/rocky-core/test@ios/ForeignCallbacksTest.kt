@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.posix.pthread_self
import rocky.internal.native.bindingLiveHandles
import kotlin.test.*

class ForeignCallbacksTest {
    @Test fun publicSecretStoreAndTransportEnrollThroughRust() = runBlocking {
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
            val streams = atomic(0)
            val ownerTransport = MemoryTransport("owner", streams)
            val memberTransport = MemoryTransport("member", streams)
            ownerTransport.peer = memberTransport
            memberTransport.peer = ownerTransport
            val root = NSTemporaryDirectory() + "rocky-foreign-" + NSUUID().UUIDString
            val definition = schema("foreigncallbacks", 1) {
                table("items") {
                    text("key")
                    primaryKey("key")
                }
            }
            val owner = RockyDatabase.create("$root/owner", definition, PeerTransport.Custom(ownerTransport), secrets, automaticSync = false)
            var member: RockyDatabase? = null
            try {
                val invitation = owner.pairing(GrantProfile.Writer(setOf("items")))
                member = RockyDatabase.join("$root/member", definition, invitation.uri, PeerTransport.Custom(memberTransport), automaticSync = false)
                assertEquals(owner.domain, member.domain)
                assertTrue(calls.value > 0)
                assertTrue(streams.value > 0)
            } finally {
                member?.close()
                owner.close()
                ownerTransport.inbound.close()
                memberTransport.inbound.close()
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
            while (bindingLiveHandles() != 0) yield()
            assertEquals(0, bindingLiveHandles())
        }
    }
}

private class MemoryTransport(private val name: String, private val calls: kotlinx.atomicfu.AtomicInt) : StreamTransport {
    lateinit var peer: MemoryTransport
    val inbound = Channel<InboundStream>(Channel.UNLIMITED)
    override val kind = "kotlin-phase17"
    override suspend fun address(): String = name
    override suspend fun accept(): InboundStream? = inbound.receiveCatching().getOrNull()
    override suspend fun connect(address: String, protocol: String): ByteStream {
        check(address == peer.name)
        val outgoing = Channel<ByteArray>(Channel.UNLIMITED)
        val incoming = Channel<ByteArray>(Channel.UNLIMITED)
        peer.inbound.send(InboundStream(protocol, MemoryStream(outgoing, incoming, calls)))
        return MemoryStream(incoming, outgoing, calls)
    }
}
private class MemoryStream(private val incoming: Channel<ByteArray>, private val outgoing: Channel<ByteArray>, private val calls: kotlinx.atomicfu.AtomicInt) : ByteStream {
    private var pending = ByteArray(0)
    override suspend fun read(max: Int): ByteArray {
        calls.incrementAndGet()
        if (pending.isEmpty()) pending = incoming.receiveCatching().getOrNull() ?: return ByteArray(0)
        val count = minOf(max, pending.size)
        return pending.copyOfRange(0, count).also { pending = pending.copyOfRange(count, pending.size) }
    }
    override suspend fun write(bytes: ByteArray) {
        calls.incrementAndGet()
        outgoing.send(bytes.copyOf())
    }
    override suspend fun finish() {
        outgoing.close()
    }
    override suspend fun close() {
        incoming.cancel()
        outgoing.cancel()
    }
}
