package org.example.examples.shopping.qualification

import androidx.room.Room
import androidx.room.useReaderConnection
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.example.examples.shopping.data.ShoppingRepository
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.tips.HouseholdTips
import org.example.examples.shopping.data.tips.TipsState
import rocky.*
import rocky.room.setRockyDatabase
import java.io.File

/** Marks a reply line; native logging shares stdout. */
internal const val REPLY = "@@rocky-device@@ "

/**
 * One shopping device in its own process: the app's Room repository on Rocky's file over isolated iroh. It reads one JSON
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
    private var file: File? = null
    private var runtime: RockyDatabase? = null
    private var room: ShoppingDatabase? = null
    private var directory: File? = null
    private var attached: RockyNetworkService? = null
    private val database get() = runtime ?: error("no open device")
    private val repository get() = ShoppingRepository(room ?: error("no open device"), database)
    private fun tips(sealAfter: ULong = HouseholdTips.SEAL_AFTER) = HouseholdTips(database, { Room.databaseBuilder<ShoppingDatabase>(it) }, directory!!.path, sealAfter)

    private suspend fun opened(directory: File, open: suspend () -> RockyDatabase): JsonElement {
        val opened = open()
        runtime = opened
        this.directory = directory
        file = File(directory, "rocky.sqlite")
        room = Room.databaseBuilder<ShoppingDatabase>(File(directory, "shopping").absolutePath)
            .setRockyDatabase(opened).build()
        tips().keepSeals()
        return buildJsonObject {
            put("node", opened.session().node)
            put("issuer", opened.isIssuer)
            put("generation", opened.generation())
        }
    }

    suspend fun handle(request: JsonObject): JsonElement {
        fun loopback() = PeerTransport.Iroh.isolated(request.text("listen"))
        fun directory() = File(request.text("directory"))
        fun schema() = appSchema(request["app"]?.jsonPrimitive?.int ?: CURRENT_APP)
        fun coordinator() = request["coordinator"]?.jsonPrimitive?.contentOrNull?.let { ServiceCoordinator(File(it).readText()) }
        val list = { request.long("list") }
        return when (request.text("op")) {
            "schema" -> Json.parseToJsonElement(ShoppingSchema.json())

            "create" -> opened(directory()) {
                RockyDatabase.create(directory(), schema(), loopback(), automaticSync = false, coordinator = coordinator()).also {
                    it.tips.createBookmark("main")
                    it.tips.follow("main")
                    it.tips.setGenesisMinimum(1u)
                }
            }

            "join" -> opened(directory()) {
                RockyDatabase.join(directory(), schema(), request.text("invitation"), loopback(), automaticSync = false, coordinator = coordinator())
            }

            // Opens without a network: what opening publishes stays here until `attach`.
            "resumeOffline" -> opened(directory()) {
                RockyDatabase.open(directory(), schema(), null, automaticSync = false, coordinator = coordinator(), confirmation = request["confirmation"]?.jsonPrimitive?.contentOrNull)
            }

            "attach" -> {
                attached = database.attach(loopback())
                JsonNull
            }

            "reopen" -> {
                close()
                opened(directory()) {
                    RockyDatabase.resume(directory(), schema(), loopback(), automaticSync = false, coordinator = coordinator(), confirmation = request["confirmation"]?.jsonPrimitive?.contentOrNull)
                }
            }

            "generation" -> database.generation()

            "inspect" -> JsonPrimitive(database.tips.inspect().toString())

            // As the app's approvals do: the household's trusted identities register with the coordination service.
            "register" -> {
                val trusted = database.devices().filter { it.profile == GrantProfile.Issuer && !it.revoked }.map { it.user } + database.session().principal
                database.service(randomTenant()).register(RockyDatabase.newHandle(), trusted.distinct().sorted())
                JsonNull
            }

            "tips" -> tips().refresh().json()

            "maintain" -> {
                val round = tips(request["sealAfter"]?.jsonPrimitive?.longOrNull?.toULong() ?: HouseholdTips.SEAL_AFTER).maintain()
                buildJsonObject {
                    put("sealed", round.sealed)
                    put("compacted", round.compaction.compacted)
                    put("finalized", round.compaction.finalized)
                    put("pruned", round.compaction.pruned.toLong())
                    put("blocked", JsonArray(round.compaction.blocked.map { JsonPrimitive(it.reason) }.distinct()))
                    put("uncompacted", round.compaction.uncompacted.size)
                }
            }

            "retention" -> database.retention(request.text("collection"), maintain = request["maintain"]?.jsonPrimitive?.boolean ?: false)

            "seal" -> JsonPrimitive(tips().seal().tip)

            "branch" -> JsonPrimitive(tips().branch(request.text("name")))

            "follow" -> JsonPrimitive(tips().follow(request.text("name")).name)

            "leave" -> {
                tips().leave(request.text("name"), request["delete"]?.jsonPrimitive?.boolean ?: false)
                JsonNull
            }

            "rollBack" -> JsonPrimitive(tips().rollBack(request.text("seal")))

            "snapshot" -> JsonArray(
                tips().snapshot(request.text("seal")).map { snapshot ->
                    buildJsonObject {
                        put("title", snapshot.title)
                        put("items", JsonArray(snapshot.items.map { JsonPrimitive(it.title + if (it.completed) " [x]" else "") }))
                    }
                },
            )

            "held" -> JsonArray(
                database.heldChanges().changes.map { change ->
                    buildJsonObject {
                        put("id", change.id)
                        put(
                            "rows",
                            JsonArray(
                                change.rows.map { row ->
                                    JsonPrimitive("${row.kind} ${row.table} ${row.fields.firstOrNull { it.name == "title" }?.value ?: ""}".trim())
                                },
                            ),
                        )
                    }
                },
            )

            "reauthor" -> {
                tips().reauthor(request.text("id"))
                JsonNull
            }

            // What compaction leaves in the file, read by another process: tombstones and operations of every scope.
            "storage" -> buildJsonObject {
                put("rows", scalar("SELECT count(*) FROM secure_rows"))
                put("tombstones", scalar("SELECT COALESCE(SUM(tombstones),0) FROM secure_scope_state") + scalar("SELECT count(*) FROM secure_rows WHERE fields IS NULL"))
                put("operations", scalar("SELECT count(*) FROM secure_operations"))
                put("stubs", scalar("SELECT count(*) FROM secure_stubs"))
                put("existence", scalar("SELECT count(*) FROM secure_existence_log"))
                put("history", scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name LIKE 'secure_history_%'"))
                val self = database.tips.inspect().nodes.firstOrNull { it.keptTips != null }
                put("tips", JsonArray(self?.keptTips.orEmpty().map(::JsonPrimitive)))
            }

            // Approvals as the app writes them: a request is an ordinary row, a decision a coordinated write.
            "ask" -> {
                val key = RockyDatabase.newHandle()
                val self = database.session().node
                database.write { execute("INSERT INTO requests(key,title,note,requester,id) VALUES(?,?,?,?,?)", key, request.text("title"), "", self, nextId("requests")) }
                JsonPrimitive(key)
            }

            "decide" -> {
                val key = request.text("key")
                val self = database.session().node
                database.coordinated {
                    check(query("SELECT key FROM requests WHERE key=?", key).isNotEmpty()) { "request $key has not arrived" }
                    execute("INSERT INTO decisions(key,approved,decider,id) VALUES(?,?,?,?)", key, true, self, nextId("decisions"))
                }
                JsonPrimitive(database.coordinatedStatus().position)
            }

            "decisions" -> JsonArray(room!!.approvals().decisions().first().map { JsonPrimitive(it.key) })

            "coordinated" -> JsonPrimitive(database.coordinatedStatus().position)

            "prepare" -> Credentials.prepare(directory().path)

            "grantWriter" -> database.grant(
                request.getValue("device").jsonObject,
                NodeKind.Full.profile,
                authors = ShoppingSchema.tables.size,
            )

            "install" -> opened(directory()) {
                RockyDatabase.install(directory().path, ShoppingSchema, request.getValue("supplied").jsonObject, transport = loopback())
            }

            "resume" -> opened(directory()) { RockyDatabase.resume(directory(), ShoppingSchema, loopback(), automaticSync = false) }

            "pairing" -> JsonPrimitive(
                when (val profile = request.text("profile")) {
                    "writer" -> database.pairing(NodeKind.Full.profile)
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
                database.setNetworkingEnabled(request.getValue("online").jsonPrimitive.boolean)
                JsonNull
            }

            "sync" -> {
                val report = database.syncOnce()
                buildJsonObject {
                    put("skip", report.skip?.name)
                    put(
                        "peers",
                        JsonArray(
                            report.peers.map { peer ->
                                buildJsonObject {
                                    put("node", peer.node)
                                    put("skip", peer.skip?.name)
                                    put(
                                        "attempts",
                                        JsonArray(
                                            peer.attempts.map { attempt ->
                                                buildJsonObject {
                                                    put("operation", attempt.operation.name)
                                                    put("stage", attempt.stage.name)
                                                    put("outcome", attempt.outcome?.name)
                                                    put("error", attempt.failure?.detail)
                                                }
                                            },
                                        ),
                                    )
                                }
                            },
                        ),
                    )
                }
            }

            "createList" -> JsonPrimitive(repository.createList(request.text("title")))

            "addCategory" -> JsonPrimitive(repository.addCategory(list(), request.text("name")))

            "add" -> JsonPrimitive(repository.add(list(), request.text("title"), setOfNotNull(request.longOrNull("category"))))

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

            "share" -> {
                repository.share(list(), request.getValue("recipients").jsonArray.map { it.jsonPrimitive.content })
                JsonNull
            }

            "shareWithDevices" -> JsonArray(repository.shareWithDevices(list()).map(::JsonPrimitive))

            // As the Devices tab names a device: its registry label and the trusted-only row.
            "label" -> {
                Household(room!!, database, database.session().node).record(request.text("device"), request.text("name"), NodeKind.Full)
                JsonNull
            }

            "deliveries" -> JsonObject(repository.deliveries(list()).associate { it.node to JsonPrimitive(it.state.name) })

            "absentTables" -> {
                val tables = if (database.isIssuer) listOf("keys", "identities") else listOf("devices", "keys", "identities")
                val checks = tables.associateWith { table ->
                    val error = runCatching {
                        room!!.useReaderConnection { reader -> reader.usePrepared("SELECT * FROM $table") { it.step() } }
                    }.exceptionOrNull()
                    JsonPrimitive(error?.message ?: "table exists")
                }
                val process = ProcessBuilder("sqlite3", "-readonly", file!!.path, "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('devices','keys','identities')").redirectErrorStream(true).start()
                val physical = process.inputStream.bufferedReader().readText()
                check(process.waitFor() == 0) { "SQLite inspection failed: $physical" }
                buildJsonObject {
                    put("sql", JsonObject(checks))
                    put("physical", JsonArray(physical.lines().filter { it.isNotBlank() }.map(::JsonPrimitive)))
                }
            }

            "lists" -> JsonArray(room!!.lists().observeAll().first().map { JsonPrimitive("${it.id}:${it.title}") })

            "items" -> {
                val links = room!!.itemCategories().observe(list()).first().groupBy({ it.item }, { it.category })
                JsonArray(
                    room!!.items().observeAll(list()).first().sortedBy { it.title }.map { item ->
                        buildJsonObject {
                            put("id", item.id)
                            put("title", item.title)
                            put("completed", item.completed)
                            links[item.id]?.sorted()?.let { put("categories", JsonArray(it.map(::JsonPrimitive))) }
                        }
                    },
                )
            }

            "status" -> database.partitions.status("list", database.list(list()).values)

            "revoke" -> {
                database.revoke(request.text("node"))
                JsonNull
            }

            "renew" -> JsonPrimitive(database.renew().revocations.statements)

            "close" -> JsonNull

            else -> error("unknown op ${request.text("op")}")
        }
    }

    private fun scalar(sql: String): Long {
        val process = ProcessBuilder("sqlite3", "-readonly", file!!.path, sql).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText().trim()
        check(process.waitFor() == 0) { "SQLite inspection failed: $output" }
        return output.toLong()
    }

    suspend fun close() {
        try {
            room?.close()
        } finally {
            try {
                attached?.close()
            } finally {
                runtime?.close()
            }
        }
        attached = null
        room = null
        runtime = null
    }
}

private fun randomTenant() = ByteArray(32).also { java.security.SecureRandom().nextBytes(it) }

private fun TipsState.json() = buildJsonObject {
    put("bookmark", bookmark)
    put("tip", tip)
    put("operationsSinceSeal", operationsSinceSeal.toLong())
    put("bookmarks", JsonObject(bookmarks.associate { it.name to JsonPrimitive(it.tip) }))
    put("losers", JsonObject(losers.associate { it.name to JsonPrimitive(it.tip) }))
    put(
        "path",
        JsonArray(
            path.map {
                buildJsonObject {
                    put("id", it.id)
                    put("tip", it.tip)
                    put("held", it.held?.name)
                    put("closed", it.closed)
                }
            },
        ),
    )
    put("held", held.size)
    put("pendingFollow", pendingFollow)
}
