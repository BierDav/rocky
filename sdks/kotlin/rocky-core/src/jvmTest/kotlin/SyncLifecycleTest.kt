package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import java.io.File
import java.nio.file.Files
import java.util.concurrent.ConcurrentHashMap
import kotlin.test.*

class SyncLifecycleTest {
    private class Pipes {
        val listeners = ConcurrentHashMap<String, Channel<InboundStream>>()
        val dials = Channel<Unit>(Channel.UNLIMITED)

        @Volatile var stalled = false
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
        private val inbound = Channel<InboundStream>(8).also { pipes.listeners[name] = it }
        override val kind = "lifecycle-test"
        override suspend fun address() = name
        override suspend fun accept() = inbound.receiveCatching().getOrNull()
        override suspend fun connect(address: String, protocol: String): ByteStream {
            if (pipes.stalled && name == "a") {
                pipes.dials.send(Unit)
                awaitCancellation()
            }
            val there = Channel<ByteArray>(8)
            val back = Channel<ByteArray>(8)
            pipes.listeners.getValue(address).send(InboundStream(protocol, Pipe(there, back)))
            return Pipe(back, there)
        }
    }

    @Test
    fun backgroundCancelsNativeDialAndForegroundStartsImmediately() = runBlocking {
        val root = Files.createTempDirectory("rocky-sync-lifecycle").toFile()
        val schema = schema("lifecycle") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val pipes = Pipes()
        val a = RockyDatabase.create(File(root, "a"), schema, PeerTransport.Custom(Network(pipes, "a")))
        var b: RockyDatabase? = null
        try {
            b = RockyDatabase.join(File(root, "b"), schema, a.pairing(GrantProfile.Issuer).uri, PeerTransport.Custom(Network(pipes, "b")), automaticSync = false)
            a.addPeer(b.contact().uri)
            pipes.stalled = true
            a.setOnline(true)
            withTimeout(5_000) { pipes.dials.receive() }
            val before = System.nanoTime()
            withTimeout(1_000) { a.enterBackground() }
            val background = System.nanoTime() - before
            assertTrue(background < 1_000_000_000)
            while (pipes.dials.tryReceive().isSuccess) {}
            val foreground = System.nanoTime()
            // Foregrounding itself enables the worker; no timer or explicit sync is supplied.
            withTimeout(1_000) {
                a.enterForeground()
                pipes.dials.receive()
            }
            println("background cancelled in ${background / 1_000_000} ms; foreground dial in ${(System.nanoTime() - foreground) / 1_000_000} ms")
            withTimeout(1_000) { a.enterBackground() }
        } finally {
            pipes.stalled = false
            b?.close()
            a.close()
            root.deleteRecursively()
        }
    }
}
