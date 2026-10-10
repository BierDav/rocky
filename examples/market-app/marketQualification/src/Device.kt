package org.example.examples.market.qualification

import androidx.room.Room
import androidx.sqlite.SQLiteException
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.*
import org.example.examples.market.Market
import org.example.examples.market.MarketDatabase
import org.example.examples.market.MarketNode
import org.example.examples.market.MarketPlatform
import org.example.examples.market.MarketSchema
import org.example.examples.market.Opening
import org.example.examples.market.Reserve
import org.example.examples.market.ReserveArguments
import org.example.examples.market.Restock
import org.example.examples.market.reserve
import org.example.examples.market.restock
import rocky.*
import java.io.File
import java.io.FileOutputStream
import java.security.SecureRandom
import java.util.Base64

/** Marks a reply line; native logging shares stdout. */
internal const val REPLY = "@@market-device@@ "

/**
 * One market device in its own process: the app's [MarketNode] with Room on Rocky's file, over iroh on loopback with
 * the coordination service of `bundle`. It reads one JSON request per stdin line and answers each on a [REPLY] line.
 * Closing stdin closes the device; the harness may also kill it with SIGKILL.
 */
fun main() = runBlocking {
    val device = DeviceProcess()
    try {
        while (true) {
            val line = readlnOrNull() ?: break
            val request = Json.parseToJsonElement(line).jsonObject
            val reply = try {
                buildJsonObject { put("ok", device.handle(request)) }
            } catch (error: RockyException) {
                buildJsonObject {
                    put("error", "${error.code}: ${error.message}")
                    put("code", error.code)
                }
            } catch (error: Exception) {
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
private val base64 = Base64.getEncoder()
private val unbase64 = Base64.getDecoder()

/** Owner-only files: the market's tenant key and pending controls. */
private class FileSecrets(private val directory: File) : SecretStore {
    override fun load(name: String): ByteArray? = File(directory, name).takeIf { it.isFile }?.readBytes()
    override fun store(name: String, secret: ByteArray) {
        directory.mkdirs()
        val temporary = File(directory, "$name.tmp")
        FileOutputStream(temporary).use { out ->
            out.write(secret)
            out.fd.sync()
        }
        check(temporary.renameTo(File(directory, name))) { "could not store $name" }
    }
    override fun delete(name: String) {
        File(directory, name).delete()
    }
}

/**
 * Records each handler call durably before it decides, so a scenario can count calls per identity across kills.
 * Counting is the harness's; the handlers it wraps are the app's.
 */
private class Calls(private val file: File) {
    fun record(line: String) = synchronized(this) {
        FileOutputStream(file, true).use { out ->
            out.write("$line\n".toByteArray())
            out.fd.sync()
        }
    }
    fun handlers(): List<Registration<*, *, *>> = listOf(
        Reserve handledBy { context, arguments ->
            record("reserve ${arguments.hold}")
            reserve.handler.handle(context, arguments)
        },
        Restock handledBy { context, arguments ->
            record("restock ${arguments.item}")
            restock.handler.handle(context, arguments)
        },
    )
}

private fun ticket(ticket: Ticket<*, *>?): JsonElement = ticket?.let {
    buildJsonObject {
        put("handle", it.handle)
        put("state", it.state.name)
        put("sequence", it.sequence)
        put(
            "outcome",
            when (val outcome = it.outcome) {
                null -> JsonNull
                is Outcome.Accepted -> JsonPrimitive("accepted ${outcome.value}")
                is Outcome.Rejected -> JsonPrimitive("rejected ${outcome.value}")
            },
        )
        put("error", it.error?.let { error -> "${error.code}: ${error.message}" })
        put("code", it.error?.code)
        put("retryable", it.error?.retryable)
    }
} ?: JsonNull

private class DeviceProcess {
    private var node: MarketNode? = null
    private var lastSync: ConnectionStatus? = null
    private val market get() = (node ?: error("no open device")).market
    private val rocky get() = market.rocky

    private suspend fun open(request: JsonObject, opening: Opening): JsonElement {
        val directory = File(request.text("directory"))
        val transport = PeerTransport.Iroh("disabled", discovery = false, bind = request.text("listen"))
        val calls = Calls(File(directory, "calls.log"))
        val random = SecureRandom()
        val platform = MarketPlatform(
            trustBundle = File(request.text("bundle")).readText(),
            open = { how, commands ->
                // The app's handlers, counted; everything else as the app composes it.
                val counted = CommandRuntime(commands.coordinator, calls.handlers(), commands.handlerDeadlineMillis)
                val rocky = File(directory, "rocky")
                when (how) {
                    Opening.Create -> RockyDatabase.create(rocky, MarketSchema, transport, automaticSync = false, commands = counted)
                    is Opening.Join -> RockyDatabase.join(rocky, MarketSchema, how.invitation, transport, automaticSync = false, commands = counted)
                    Opening.Resume -> RockyDatabase.resume(rocky, MarketSchema, transport, automaticSync = false, commands = counted)
                }
            },
            room = { Room.databaseBuilder<MarketDatabase>(File(directory, "market").absolutePath) },
            secrets = FileSecrets(File(directory, "app-secrets")),
            secureRandom = { ByteArray(it).also(random::nextBytes) },
        )
        val opened = MarketNode.open(platform, opening)
        node = opened
        return buildJsonObject {
            put("node", opened.rocky.session().node)
            put("issuer", opened.rocky.isIssuer)
        }
    }

    suspend fun handle(request: JsonObject): JsonElement {
        val handle = { request.text("handle") }
        val identities = { name: String -> request[name]?.jsonArray?.map { CommandIdentity.decode(it.jsonPrimitive.content) } }
        return when (val op = request.text("op")) {
            "create" -> open(request, Opening.Create)

            "join" -> open(request, Opening.Join(request.text("invitation")))

            "resume" -> open(request, Opening.Resume)

            "pairing" -> JsonPrimitive(
                when (val profile = request.text("profile")) {
                    "buyer" -> rocky.pairing(Market.Buyer)
                    "issuer" -> rocky.pairDevice()
                    "custody" -> rocky.pairing(GrantProfile.Custody(MarketSchema.tables.map { it.name }.toSet()))
                    else -> error("unknown profile $profile")
                }.uri,
            )

            "routeEnrollment" -> JsonPrimitive(rocky.routeEnrollment(request.text("invitation"), request.text("contact")))

            "contact" -> JsonPrimitive(rocky.contact().uri)

            "addPeer" -> JsonPrimitive(rocky.addPeer(request.text("contact")))

            "retainOnly" -> {
                val kept = rocky.addPeer(request.text("contact"))
                rocky.peers().filter { it.isConfigured && it.node != kept }.forEach { rocky.removePeer(it.node) }
                JsonNull
            }

            "diagnostics" -> buildJsonObject {
                put("peers", rocky.peers().toString())
                put("network", rocky.network().toString())
                put("error", lastSync?.error)
                put("results", withTimeoutOrNull(1_000) { rocky.activity().first { it is RockyEvent.ResultsFailed } }?.toString())
            }

            "sync" -> JsonPrimitive(rocky.syncOnce().also { lastSync = it }.state.name)

            "identity" -> JsonPrimitive(rocky.commandIdentity().encoded)

            "publish" -> JsonPrimitive(market.publishIdentity().encoded)

            "published" -> JsonPrimitive(market.published().size)

            "control" -> market.control(identities("executors"), identities("invokers")).let {
                buildJsonObject {
                    put("revision", it.revision)
                    put("tenant", it.tenant)
                }
            }

            "closeDomain" -> {
                market.closeDomain()
                JsonNull
            }

            "listing" -> {
                market.addListing(request.text("key"), request.text("title"))
                JsonNull
            }

            "memo" -> JsonPrimitive(market.addMemo(request.text("listing"), request.text("note")))

            "memos" -> JsonArray(market.memos().first().map { JsonPrimitive(it.note) })

            "offers" -> JsonArray(market.offers().first().map { JsonPrimitive("${it.key}=${it.remaining}") })

            "holds" -> JsonArray(market.holds().first().map { JsonPrimitive("${it.key}:${it.item}=${it.quantity}") })

            "sequence" -> JsonPrimitive(rocky.confirmedRows("stock").sequence)

            "submit" -> ticket(rocky.submit(Reserve, ReserveArguments(request.text("item"), request.long("quantity"), request.text("hold")), handle()))

            "restock" -> ticket(market.restock(request.text("item"), request.long("quantity"), handle()))

            "run" -> ticket(market.retry(handle()))

            "ticket" -> ticket(market.reservation(handle()))

            "envelope" -> JsonPrimitive(base64.encodeToString(rocky.envelope(handle())))

            "execute" -> JsonPrimitive(base64.encodeToString(rocky.execute(unbase64.decode(request.text("envelope")))))

            "settle" -> ticket(rocky.settle(Reserve, handle(), unbase64.decode(request.text("answer"))))

            "acknowledge" -> {
                rocky.acknowledgeCommands()
                JsonNull
            }

            // Writes to command-controlled rows: each must fail. The reply names how.
            "overwriteStock" -> {
                val stock = market.room.stock().observe().first().first()
                try {
                    market.room.stock().overwrite(stock.copy(remaining = stock.remaining + 100))
                    JsonPrimitive("written")
                } catch (error: SQLiteException) {
                    JsonPrimitive("refused: ${error.message}")
                }
            }

            "sqlWrite" -> try {
                rocky.write { execute(request.text("sql")) }
                JsonPrimitive("written")
            } catch (error: RockyException) {
                JsonPrimitive("refused: ${error.code}")
            }

            "close" -> JsonNull

            else -> error("unknown op $op")
        }
    }

    suspend fun close() {
        node?.close()
        node = null
    }
}
