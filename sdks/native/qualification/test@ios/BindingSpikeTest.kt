@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky.binding

import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.posix.pthread_self
import rocky.internal.native.*
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class BindingSpikeTest {
    @Test
    fun rustThreadCallbacksAndAsyncForeignStreams() = runBlocking {
        withTimeout(30_000) {
            val root = NSTemporaryDirectory() + "rocky-binding-" + NSUUID().UUIDString
            val caller = pthread_self()
            val secrets = Secrets { pthread_self() == caller }
            val ownerTransport = Transport("owner")
            val memberTransport = Transport("member")
            ownerTransport.peer = memberTransport
            memberTransport.peer = ownerTransport
            val ownerNetwork = NativeNetwork.foreign(ownerTransport)
            val memberNetwork = NativeNetwork.foreign(memberTransport)
            val owner = NativeDatabase.create("$root/owner", SCHEMA, secrets, ownerNetwork, null, null)
            val session = owner.session()
            var member: NativeDatabase? = null
            try {
                val status = call(owner, session, buildJsonObject { put("type", "session") })
                assertTrue(status.getValue("node").jsonPrimitive.content.isNotEmpty())
                assertTrue(secrets.rustThreadCalls.value > 0)
                val pairing = call(owner, session, buildJsonObject {
                    put("type", "pairing")
                    put("profile", buildJsonObject {
                        put("kind", "writer")
                        put("scopes", JsonArray(listOf(JsonPrimitive("items"))))
                    })
                })
                member = NativeDatabase.join("$root/member", SCHEMA, pairing.getValue("uri").jsonPrimitive.content, memberNetwork, null, null)
                assertEquals(owner.scope(), member.scope())
                assertTrue(memberTransport.connections.value > 0)
                assertTrue(Stream.reads.value > 0 && Stream.writes.value > 0)
            } finally {
                member?.let { it.shutdown(); it.destroy() }
                session.shutdown()
                session.destroy()
                owner.shutdown()
                owner.destroy()
                ownerNetwork.destroy()
                memberNetwork.destroy()
                ownerTransport.inbound.close()
                memberTransport.inbound.close()
                NSFileManager.defaultManager.removeItemAtPath(root, null)
            }
        }
    }
}

private suspend fun call(database: NativeDatabase, session: NativeDatabaseSession, request: JsonObject): JsonObject {
    val frame = buildJsonObject {
        put("version", 1)
        put("scope", Json.parseToJsonElement(database.scope()))
        put("request", request)
    }
    val reply = Json.parseToJsonElement(session.call(frame.toString())).jsonObject
    assertEquals("ok", reply.getValue("status").jsonPrimitive.content, reply.toString())
    return reply.getValue("value").jsonObject
}

private class Secrets(private val onCallerThread: () -> Boolean) : NativeSecretStore {
    private val lock = ReentrantLock()
    private val values = mutableMapOf<String, ByteArray>()
    val rustThreadCalls = atomic(0)
    private fun called() {
        if (!onCallerThread()) rustThreadCalls.incrementAndGet()
    }
    override fun load(name: String): ByteArray? = lock.withLock {
        called()
        values[name]?.copyOf()
    }
    override fun store(name: String, secret: ByteArray) = lock.withLock {
        called()
        values[name] = secret.copyOf()
    }
    override fun delete(name: String) = lock.withLock {
        called()
        values.remove(name)
        Unit
    }
}

private class Transport(private val address: String) : ForeignTransport {
    lateinit var peer: Transport
    val inbound = Channel<InboundStream>(Channel.UNLIMITED)
    val connections = atomic(0)
    override fun kind(): String = "kotlin-spike"
    override suspend fun address(): String = address
    override suspend fun accept(): InboundStream? = inbound.receiveCatching().getOrNull()
    override suspend fun connect(address: String, protocol: String): OutboundStream {
        check(address == peer.address)
        connections.incrementAndGet()
        val outgoing = Channel<ByteArray>(Channel.UNLIMITED)
        val incoming = Channel<ByteArray>(Channel.UNLIMITED)
        peer.inbound.send(InboundStream(protocol, Stream(outgoing, incoming)))
        return OutboundStream(Stream(incoming, outgoing))
    }
}

private class Stream(private val incoming: Channel<ByteArray>, private val outgoing: Channel<ByteArray>) : ByteStream {
    private var pending = ByteArray(0)
    override suspend fun read(max: UInt): ByteArray {
        reads.incrementAndGet()
        if (pending.isEmpty()) pending = incoming.receiveCatching().getOrNull() ?: return ByteArray(0)
        val count = minOf(max.toInt(), pending.size)
        return pending.copyOfRange(0, count).also { pending = pending.copyOfRange(count, pending.size) }
    }
    override suspend fun write(bytes: ByteArray) {
        writes.incrementAndGet()
        outgoing.send(bytes.copyOf())
    }
    override suspend fun finish() { outgoing.close() }
    override suspend fun abandon() {
        incoming.close()
        outgoing.close()
    }
    companion object {
        val reads = atomic(0)
        val writes = atomic(0)
    }
}

private const val SCHEMA = """{"application":"binding","version":1,"format":4,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"""
