package org.example.examples.shopping.qualification

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.*
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.LinkState
import rocky.Peer
import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.PrintWriter
import java.net.InetAddress
import java.net.ServerSocket
import java.time.Instant
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList

/** Wall-clock microseconds; the driver corrects the phone's clock by the offset it measures over USB. */
fun epochMicros(): Long = Instant.now().let { it.epochSecond * 1_000_000 + it.nano / 1_000 }

/**
 * One shopping device for the live-link qualification: the app's [ShoppingNode] with its household work, answering
 * one JSON request per line. It records when each item title is first observed and every link transition, with
 * [epochMicros] timestamps taken on this device.
 */
class LiveLinkAgent(
    /** Builds the device's platform; `transport` carries per-run transport options, such as the phone's lookup. */
    private val platform: (directory: String, name: String, transport: JsonObject) -> ShoppingPlatform,
    private val log: (String) -> Unit,
    /** This process's own network counters, as the platform measures them. */
    private val traffic: () -> JsonObject,
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var node: ShoppingNode? = null
    private val opened get() = node ?: error("no open device")
    private val seen = ConcurrentHashMap<String, Long>()
    private val lists = ConcurrentHashMap<Long, Long>()
    private val watched = ConcurrentHashMap.newKeySet<Long>()
    private val events = CopyOnWriteArrayList<JsonObject>()
    private val links = CopyOnWriteArrayList<JsonObject>()

    private suspend fun open(open: suspend () -> ShoppingNode): JsonElement {
        check(node == null) { "a device is already open" }
        val opened = open()
        node = opened
        scope.launch {
            opened.rocky.activity().collect { event ->
                val line = buildJsonObject {
                    put("at", epochMicros())
                    put("type", event::class.simpleName)
                    put("sequence", event.sequence)
                    put("event", event.toString())
                }
                events += line
                log("activity $line")
            }
        }
        scope.launch {
            opened.rocky.observeStatus()
                .map { readiness -> readiness.network.peers.associate { it.node to it.link.state } }
                .distinctUntilChanged()
                .collect { states ->
                    val line = buildJsonObject {
                        put("at", epochMicros())
                        put("links", JsonObject(states.mapValues { JsonPrimitive(it.value.name) }))
                    }
                    links += line
                    log("links $line")
                }
        }
        scope.launch {
            opened.repository.lists().collect { all ->
                val at = epochMicros()
                all.forEach { lists.putIfAbsent(it.id, at) }
            }
        }
        return buildJsonObject {
            put("node", opened.self)
            put("trusted", opened.isTrusted)
        }
    }

    private suspend fun status(): JsonObject {
        val readiness = opened.rocky.observeStatus().first()
        val network = opened.rocky.network()
        return buildJsonObject {
            put("at", epochMicros())
            put("networkingEnabled", readiness.network.networkingEnabled)
            put("bootstrapPending", readiness.data.bootstrapPending)
            put("revision", readiness.data.revision.toLong())
            put("inboundServed", network.inbound?.served?.toLong())
            put("inboundRefused", network.inbound?.refused?.toLong())
            put("peers", JsonArray(network.peers.map(::peer)))
        }
    }

    private fun peer(peer: Peer) = buildJsonObject {
        put("node", peer.node)
        put("configured", peer.isConfigured)
        put("source", peer.source)
        put("attempts", peer.attempts.toString())
        put("error", peer.attempts.lastOrNull()?.failure?.detail)
        put("link", peer.link.state.name)
        put("opened", peer.link.openedAtEpochSeconds?.toLong())
        put("roundTripMs", peer.link.roundTripMillis?.toLong())
        put("routes", JsonArray(peer.routes.map { JsonPrimitive("${it.transport} ${it.address}") }))
    }

    /** Suspends until every title in [titles] was observed, or [timeoutMs] passed; returns what was seen. */
    private suspend fun awaitSeen(titles: List<String>, timeoutMs: Long): JsonObject {
        withTimeoutOrNull(timeoutMs) {
            while (!titles.all(seen::containsKey)) delay(5)
        }
        return JsonObject(titles.mapNotNull { title -> seen[title]?.let { title to JsonPrimitive(it) } }.toMap())
    }

    suspend fun handle(request: JsonObject): JsonElement {
        fun text(name: String) = request.getValue(name).jsonPrimitive.content
        fun long(name: String) = request.getValue(name).jsonPrimitive.long
        fun platform() = platform(text("directory"), text("name"), request["transport"]?.jsonObject ?: JsonObject(emptyMap()))
        return when (val op = text("op")) {
            "now" -> JsonPrimitive(epochMicros())

            "create" -> open { ShoppingNode.create(platform()) }

            "join" -> open { ShoppingNode.join(platform(), PairingCode.parse(text("code"))) }

            "resume" -> open { ShoppingNode.resume(platform()) }

            "invite" -> JsonPrimitive(opened.household.invite(NodeKind.valueOf(text("kind")), text("name")).toString())

            "contact" -> JsonPrimitive(opened.rocky.contact().uri)

            "createList" -> JsonPrimitive(opened.repository.createList(text("title")))

            "lists" -> JsonObject(lists.mapKeys { it.key.toString() }.mapValues { JsonPrimitive(it.value) })

            "watch" -> {
                val list = long("list")
                if (watched.add(list)) {
                    scope.launch {
                        opened.repository.observe(list).collect { items ->
                            val at = epochMicros()
                            items.forEach { seen.putIfAbsent(it.title, at) }
                        }
                    }
                }
                JsonNull
            }

            "add" -> {
                val start = epochMicros()
                val id = opened.repository.add(long("list"), text("title"))
                buildJsonObject {
                    put("id", id)
                    put("start", start)
                    put("committed", epochMicros())
                }
            }

            "awaitSeen" -> awaitSeen(request.getValue("titles").jsonArray.map { it.jsonPrimitive.content }, long("timeoutMs"))

            "status" -> status()

            "traffic" -> traffic()

            "awaitLink" -> {
                // `NotOpen` is reached by the first status without an open link: a closed link may redial at once.
                val wanted = text("state")
                val peer = text("peer")
                fun matches(state: LinkState) = if (wanted == "NotOpen") state != LinkState.Open else state == LinkState.valueOf(wanted)
                val reached = withTimeoutOrNull(long("timeoutMs")) {
                    opened.rocky.observeStatus().first { readiness -> readiness.network.peers.any { it.node == peer && matches(it.link.state) } }
                }
                buildJsonObject {
                    put("reached", reached != null)
                    put("at", epochMicros())
                }
            }

            "events" -> JsonArray(events.drop(request["after"]?.jsonPrimitive?.int ?: 0))

            "links" -> JsonArray(links.drop(request["after"]?.jsonPrimitive?.int ?: 0))

            "removePeer" -> {
                opened.rocky.removePeer(text("node"))
                JsonNull
            }

            "background" -> {
                opened.rocky.enterBackground()
                JsonNull
            }

            "foreground" -> {
                opened.rocky.enterForeground()
                JsonNull
            }

            "close" -> {
                node?.close()
                node = null
                JsonNull
            }

            else -> error("unknown op $op")
        }
    }

    /** Answers one driver connection at a time on loopback [port]. */
    fun serve(port: Int) {
        ServerSocket(port, 4, InetAddress.getLoopbackAddress()).use { server ->
            log("serving on $port")
            while (true) {
                val socket = server.accept()
                Thread {
                    socket.use {
                        val reader = BufferedReader(InputStreamReader(socket.getInputStream()))
                        val writer = PrintWriter(socket.getOutputStream(), true)
                        while (true) {
                            val line = reader.readLine() ?: break
                            val reply = try {
                                val request = Json.parseToJsonElement(line).jsonObject
                                buildJsonObject { put("ok", runBlocking { handle(request) }) }
                            } catch (error: Throwable) {
                                log("failed $line: $error")
                                buildJsonObject { put("error", "${error::class.simpleName}: ${error.message}") }
                            }
                            writer.println(reply)
                        }
                    }
                }.start()
            }
        }
    }
}
