package org.example.examples.shopping.qualification

import androidx.room.Room
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.example.examples.shopping.*
import rocky.*
import rocky.room.setRockyDatabase
import java.io.File
import java.util.Base64

/** Marks a reply line; native logging shares stdout. */
internal const val REPLY = "@@rocky-device@@ "

/**
 * One shopping device in its own process: the app's Room repository on Rocky's file over TCP. It reads one JSON
 * request per stdin line and answers each on a [REPLY] line. Closing stdin closes the device.
 */
fun main() = runBlocking {
    val device = DeviceProcess()
    try {
        while (true) {
            val line = readlnOrNull() ?: break
            val request = Json.parseToJsonElement(line).jsonObject
            val reply = try {
                buildJsonObject { put("ok", device.handle(request)) }
            } catch (error: Throwable) {
                buildJsonObject { put("error", "${error::class.simpleName}: ${error.message}") }
            }
            println(REPLY + reply)
            System.out.flush()
            if (request.text("op") == "close") break
        }
    } finally {
        device.close()
    }
}

private fun JsonObject.text(name: String) = getValue(name).jsonPrimitive.content
private fun JsonObject.long(name: String) = getValue(name).jsonPrimitive.long
private fun JsonObject.longOrNull(name: String) = get(name)?.jsonPrimitive?.longOrNull

private class DeviceProcess {
    private var runtime: RockyDatabase? = null
    private var room: ShoppingDatabase? = null
    private val database get() = runtime ?: error("no open device")
    private val repository get() = ShoppingRepository(room ?: error("no open device"), database)

    private suspend fun opened(directory: File, open: suspend () -> RockyDatabase): JsonElement {
        val opened = open()
        runtime = opened
        room = Room.databaseBuilder<ShoppingDatabase>(File(directory, "shopping").absolutePath)
            .setRockyDatabase(opened).build()
        return buildJsonObject {
            put("node", opened.session().node)
            put("issuer", opened.isIssuer)
        }
    }

    suspend fun handle(request: JsonObject): JsonElement {
        fun tcp() = PeerTransport.Tcp(request.text("listen"))
        fun directory() = File(request.text("directory"))
        val list = { request.long("list") }
        return when (request.text("op")) {
            "create" -> opened(directory()) {
                RockyDatabase.create(directory(), ShoppingSchema, tcp(), automaticSync = false)
            }

            "join" -> opened(directory()) {
                RockyDatabase.join(directory(), ShoppingSchema, request.text("invitation"), tcp(), automaticSync = false)
            }

            "resume" -> opened(directory()) { RockyDatabase.resume(directory(), ShoppingSchema, tcp(), automaticSync = false) }

            "pairing" -> JsonPrimitive(
                when (val profile = request.text("profile")) {
                    "writer" -> database.pairing(GrantProfile.Writer(ShoppingSchema.tables.map { it.name }.toSet()))
                    "custody" -> database.pairing(GrantProfile.Custody(ShoppingSchema.tables.map { it.name }.toSet()))
                    "issuer" -> database.pairDevice()
                    else -> error("unknown profile $profile")
                }.uri,
            )

            "contact" -> buildJsonObject {
                val contact = database.contact()
                put("uri", contact.uri)
                put("node", contact.node)
            }

            "addPeer" -> JsonPrimitive(database.addPeer(request.text("contact")))

            "removePeer" -> {
                database.removePeer(request.text("node"))
                JsonNull
            }

            "online" -> {
                database.setOnline(request.getValue("online").jsonPrimitive.boolean)
                JsonNull
            }

            "sync" -> {
                val status = database.syncOnce()
                buildJsonObject {
                    put("state", status.state.name)
                    put(
                        "peers",
                        JsonArray(
                            database.peers().map { peer ->
                                buildJsonObject {
                                    put("node", peer.node)
                                    put("issuer", peer.isIssuer)
                                    put("state", peer.state)
                                    put("error", peer.error)
                                }
                            },
                        ),
                    )
                }
            }

            "createList" -> JsonPrimitive(repository.createList(request.text("title")))

            "addCategory" -> JsonPrimitive(repository.addCategory(list(), request.text("name")))

            "add" -> JsonPrimitive(repository.add(list(), request.text("title"), request.longOrNull("category")))

            "check" -> {
                repository.check(list(), request.long("id"), request.getValue("completed").jsonPrimitive.boolean)
                JsonNull
            }

            "delete" -> {
                repository.delete(list(), request.long("id"))
                JsonNull
            }

            "deleteCategory" -> {
                repository.deleteCategory(list(), request.long("category"))
                JsonNull
            }

            "deleteList" -> {
                repository.deleteList(list())
                JsonNull
            }

            "move" -> {
                repository.move(request.long("from"), request.long("id"), request.long("to"), request.text("handle"))
                JsonNull
            }

            "share" -> JsonObject(
                repository.share(list(), request.getValue("recipients").jsonArray.map { it.jsonPrimitive.content })
                    .mapValues { JsonPrimitive(Base64.getEncoder().encodeToString(it.value)) },
            )

            "shareCodes" -> JsonObject(repository.shareWithDevices(list()).mapValues { JsonPrimitive(it.value) })

            "acceptCode" -> JsonPrimitive(repository.acceptCode(request.text("code")))

            "accept" -> {
                repository.accept(list(), Base64.getDecoder().decode(request.text("envelope")))
                JsonNull
            }

            "lists" -> JsonArray(room!!.lists().observe().first().map { JsonPrimitive("${it.id}:${it.title}") })

            "items" -> JsonArray(
                room!!.items().observe(list()).first().sortedBy { it.title }.map { item ->
                    buildJsonObject {
                        put("id", item.id)
                        put("title", item.title)
                        put("completed", item.completed)
                        item.category?.let { put("category", it) }
                    }
                },
            )

            "status" -> database.partitions.status("list", database.list(list()).values)

            "revoke" -> {
                database.revoke(request.text("node"))
                JsonNull
            }

            "renew" -> JsonPrimitive(database.renew().policyEpoch)

            "horizon" -> {
                val values = database.list(list()).values
                val table = request.text("table")
                when (val action = request.text("action")) {
                    "declare" -> database.horizons.declare(
                        table,
                        request.getValue("participants").jsonArray.map { it.jsonPrimitive.content }.toSet(),
                        values,
                    )

                    "finalize" -> database.horizons.finalize(table, values)

                    "status" -> database.horizons.status(table, values)

                    else -> error("unknown horizon action $action")
                }
            }

            "close" -> JsonNull

            else -> error("unknown op ${request.text("op")}")
        }
    }

    suspend fun close() {
        try {
            room?.close()
        } finally {
            runtime?.close()
        }
        room = null
        runtime = null
    }
}
