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
import kotlin.test.*
import kotlin.time.TimeSource

class IosSyncLifecycleTest {
    private class Pipes {
        private val lock = ReentrantLock()
        private val listeners = mutableMapOf<String, Channel<InboundStream>>()
        fun register(name: String, channel: Channel<InboundStream>) = lock.withLock { listeners[name] = channel }
        fun listener(name: String) = lock.withLock { listeners.getValue(name) }
        val dials = Channel<Unit>(Channel.UNLIMITED)

        val stalled = atomic(false)
    }
    private class Pipe(private val incoming: Channel<ByteArray>, private val outgoing: Channel<ByteArray>) : ByteStream {
        private var rest = ByteArray(0)
        override suspend fun read(max: Int): ByteArray {
            if (rest.isEmpty()) rest = incoming.receiveCatching().getOrNull() ?: return ByteArray(0)
            return rest.copyOf(minOf(max, rest.size)).also { rest = rest.copyOfRange(it.size, rest.size) }
        }
        override suspend fun write(bytes: ByteArray) = outgoing.send(bytes)
        override suspend fun finish() {
            outgoing.close()
        }
        override suspend fun close() {
            outgoing.close()
            incoming.cancel()
        }
    }
    private class Network(private val pipes: Pipes, private val name: String) : StreamTransport {
        private val inbound = Channel<InboundStream>(8).also { pipes.register(name, it) }
        override val kind = "lifecycle-test"
        override suspend fun address() = name
        override suspend fun accept() = inbound.receiveCatching().getOrNull()
        override suspend fun connect(address: String, protocol: String): ByteStream {
            if (pipes.stalled.value && name == "a") {
                pipes.dials.send(Unit)
                awaitCancellation()
            }
            val there = Channel<ByteArray>(8)
            val back = Channel<ByteArray>(8)
            pipes.listener(address).send(InboundStream(protocol, Pipe(there, back)))
            return Pipe(back, there)
        }
    }

    @Test
    fun backgroundCancelsNativeDialAndForegroundStartsImmediately() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-sync-lifecycle-" + NSUUID().UUIDString
        val schema = schema("lifecycle") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val pipes = Pipes()
        val a = RockyDatabase.create("$root/a", schema, PeerTransport.Custom(Network(pipes, "a")))
        var b: RockyDatabase? = null
        try {
            b = RockyDatabase.join("$root/b", schema, a.pairing(GrantProfile.Issuer).uri, PeerTransport.Custom(Network(pipes, "b")), automaticSync = false)
            a.addPeer(b.contact().uri)
            pipes.stalled.value = true
            a.setOnline(true)
            withTimeout(5_000) { pipes.dials.receive() }
            val before = TimeSource.Monotonic.markNow()
            withTimeout(1_000) { a.enterBackground() }
            val background = before.elapsedNow()
            assertTrue(background.inWholeMilliseconds < 1_000)
            while (pipes.dials.tryReceive().isSuccess) {}
            val foreground = TimeSource.Monotonic.markNow()
            // Foregrounding itself enables the worker; no timer or explicit sync is supplied.
            withTimeout(1_000) {
                a.enterForeground()
                pipes.dials.receive()
            }
            println("background cancelled in ${background.inWholeMilliseconds} ms; foreground dial in ${foreground.elapsedNow().inWholeMilliseconds} ms")
            withTimeout(1_000) { a.enterBackground() }
        } finally {
            pipes.stalled.value = false
            b?.close()
            a.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
