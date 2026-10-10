package org.example.examples.market.qualification

import kotlinx.serialization.json.*
import org.example.examples.market.MarketSchema
import java.io.File
import java.lang.management.ManagementFactory
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.util.concurrent.TimeUnit
import kotlin.random.Random

internal fun json(value: Any?): JsonElement = when (value) {
    null -> JsonNull
    is JsonElement -> value
    is String -> JsonPrimitive(value)
    is Number -> JsonPrimitive(value)
    is Boolean -> JsonPrimitive(value)
    is Collection<*> -> JsonArray(value.map(::json))
    else -> error("unsupported $value")
}
internal fun expect(condition: Boolean, message: () -> String) {
    if (!condition) throw AssertionError(message())
}

/** A loopback UDP port for an iroh endpoint, kept across a device's restarts so its peers still reach it. */
internal fun freeUdpPort() = DatagramSocket(InetSocketAddress("127.0.0.1", 0)).use { it.localPort }

/** A refused call, with the device's error code when it had one. */
internal class DeviceError(message: String, val code: String?) : IllegalStateException(message)

/** A ticket as a device reported it. */
internal data class TicketView(val state: String, val sequence: Long?, val outcome: String?, val error: String?, val retryable: Boolean?) {
    val resolved get() = state in setOf("Confirmed", "Applied", "Cancelled", "Retired", "Expired")
    companion object {
        fun of(value: JsonElement) = value.jsonObject.let { ticket ->
            TicketView(
                ticket.getValue("state").jsonPrimitive.content,
                ticket["sequence"]?.jsonPrimitive?.longOrNull,
                ticket["outcome"]?.jsonPrimitive?.contentOrNull,
                ticket["error"]?.jsonPrimitive?.contentOrNull,
                ticket["retryable"]?.jsonPrimitive?.booleanOrNull,
            )
        }
    }
}

/** One market device process: [start] runs it, [kill] ends it with SIGKILL and [close] closes it in order. */
internal class Device(val name: String, private val root: File, private val bundle: File) : AutoCloseable {
    private val log = File(root, "$name.log")
    private var process: Process? = null
    private var input: java.io.BufferedWriter? = null
    private var output: java.io.BufferedReader? = null
    val directory = File(root, name)
    val listen = "127.0.0.1:${freeUdpPort()}"
    lateinit var node: String

    private fun start() {
        val started = ProcessBuilder(
            listOf(ProcessHandle.current().info().command().get()) +
                ManagementFactory.getRuntimeMXBean().inputArguments.filterNot { it.startsWith("-agentlib") || it.startsWith("-javaagent") } +
                listOf("-cp", System.getProperty("java.class.path"), "org.example.examples.market.qualification.DeviceKt"),
        ).redirectError(ProcessBuilder.Redirect.appendTo(log)).start()
        process = started
        input = started.outputStream.bufferedWriter()
        output = started.inputStream.bufferedReader()
    }

    fun call(op: String, vararg args: Pair<String, Any?>): JsonElement {
        val request = buildJsonObject {
            put("op", op)
            args.forEach { (key, value) -> put(key, json(value)) }
        }
        val writer = input ?: error("$name is not running")
        writer.write(request.toString())
        writer.newLine()
        writer.flush()
        while (true) {
            val line = output?.readLine() ?: error("$name exited during $op; see $log")
            if (!line.startsWith(REPLY)) {
                log.appendText(line + "\n")
                continue
            }
            val reply = Json.parseToJsonElement(line.removePrefix(REPLY)).jsonObject
            reply["error"]?.let { throw DeviceError("$name $op: ${it.jsonPrimitive.content}", reply["code"]?.jsonPrimitive?.contentOrNull) }
            return reply.getValue("ok")
        }
    }

    /** Starts the process and opens the device with [op]: `create`, `join` or `resume`. */
    fun open(op: String, vararg args: Pair<String, Any?>) = apply {
        start()
        node = call(op, "directory" to directory.path, "listen" to listen, "bundle" to bundle.path, *args)
            .jsonObject.getValue("node").jsonPrimitive.content
    }

    /** SIGKILL: nothing is closed or flushed beyond what the device already made durable. */
    fun kill() {
        process?.let {
            it.destroyForcibly()
            it.waitFor(30, TimeUnit.SECONDS)
        }
        process = null
        input = null
        output = null
    }

    fun restart() = apply {
        kill()
        open("resume")
    }

    fun text(op: String, vararg args: Pair<String, Any?>) = call(op, *args).jsonPrimitive.content
    fun list(op: String) = call(op).jsonArray.map { it.jsonPrimitive.content }
    fun ticket(op: String, vararg args: Pair<String, Any?>) = TicketView.of(call(op, *args))
    fun contact() = text("contact")
    fun identity() = text("identity")
    fun sync() = retry(30_000, "$name sync", pause = 200) {
        text("sync").also { check(it == "Connected") { "$name sync: $it; ${call("diagnostics")}" } }
    }

    /** Handler calls this device recorded, per `reserve <hold>` or `restock <item>`. */
    fun calls(): Map<String, Int> = File(directory, "calls.log").takeIf { it.isFile }?.readLines().orEmpty().groupingBy { it }.eachCount()

    override fun close() {
        if (process?.isAlive == true) runCatching { call("close") }
        process?.let { if (!it.waitFor(30, TimeUnit.SECONDS)) it.destroyForcibly() }
        process = null
    }
}

/**
 * Three `rocky-voter` processes on loopback, provisioned as `scripts/coordinator-local` does: `rocky-voter local`,
 * each voter started with `--provision` once, then `admin initialize`. Voters can be killed and started again.
 */
internal class Voters(private val binary: File, private val root: File) : AutoCloseable {
    private val processes = mutableMapOf<Int, Process>()
    val service = File(root, "service")
    val bundle = File(service, "service/bundle.json")
    private val admin = File(service, "admin.json")
    val count = 3

    init {
        root.mkdirs()
        command(binary.path, "local", service.path, portBlock().toString())
        for (id in 1..count) start(id, provision = true)
        retry(30_000, "initialize") { command(binary.path, "admin", admin.path, "initialize") }
        leader()
    }

    fun start(id: Int, provision: Boolean = false) {
        val arguments = listOfNotNull(binary.path, File(service, "voter-$id.json").path, "--provision".takeIf { provision })
        processes[id] = ProcessBuilder(arguments).redirectErrorStream(true)
            .redirectOutput(ProcessBuilder.Redirect.appendTo(File(root, "voter-$id.log"))).start()
    }

    /** SIGKILL, as a crashed host. */
    fun kill(id: Int) {
        processes.remove(id)?.let {
            it.destroyForcibly()
            it.waitFor(30, TimeUnit.SECONDS)
        }
    }

    val running: Set<Int> get() = processes.filterValues { it.isAlive }.keys

    /** The current leader, asked of a running voter, once one is elected. */
    fun leader(): Int = retry(30_000, "a leader") {
        val voter = running.first()
        val status = Json.parseToJsonElement(command(binary.path, "admin", admin.path, "status", voter.toString())).jsonObject
        status["leader"]?.jsonPrimitive?.intOrNull?.takeIf { it in running } ?: error("no leader yet: $status")
    }

    private fun command(vararg arguments: String): String {
        val output = File(root, "voter-command.log")
        val process = ProcessBuilder(*arguments).redirectErrorStream(true).redirectOutput(output).start()
        if (!process.waitFor(60, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            error("${arguments.drop(1).take(2)} timed out")
        }
        val text = output.readText()
        check(process.exitValue() == 0) { text }
        return text
    }

    /** Three TCP ports and three UDP ports, base+1..3 and base+11..13, free when chosen. */
    private fun portBlock(): Int {
        repeat(32) {
            val base = Random.nextInt(10_000, 60_000)
            val sockets = mutableListOf<AutoCloseable>()
            try {
                for (id in 1..3) {
                    sockets += ServerSocket(base + id)
                    sockets += DatagramSocket(base + 10 + id)
                }
                return base
            } catch (_: java.net.BindException) {
                // Another block.
            } finally {
                sockets.forEach { it.close() }
            }
        }
        error("no free voter port block")
    }

    override fun close() {
        processes.keys.toList().forEach(::kill)
    }
}

/** Retries [block] until it succeeds or [millis] pass, then rethrows its last failure. */
internal fun <T> retry(millis: Long, what: String, pause: Long = 100, block: () -> T): T {
    val deadline = System.nanoTime() + millis * 1_000_000
    while (true) {
        try {
            return block()
        } catch (error: Exception) {
            if (System.nanoTime() > deadline) throw IllegalStateException("$what: ${error.message}", error)
            Thread.sleep(pause)
        }
    }
}

/** The packaged custody launcher on loopback iroh, configured as `docs/runtime/always-on-peer.md` describes. */
internal class Custody(private val root: File, private val binary: File, invitation: String, peers: List<String>) : AutoCloseable {
    private val config = File(root, "custody.json")
    private val log = File(root, "custody.log")
    private var process: Process? = null
    val contact: String

    init {
        config.writeText(
            buildJsonObject {
                put("directory", File(root, "custody").path)
                put("schema", Json.parseToJsonElement(MarketSchema.json()))
                put(
                    "transport",
                    buildJsonObject {
                        put("kind", "iroh")
                        put("relay", "disabled")
                        put("lookup", "disabled")
                        put("topic", false)
                        put("local", false)
                        put("bind", "127.0.0.1:${freeUdpPort()}")
                    },
                )
                put("invitation", invitation)
                put("peers", JsonArray(peers.map(::JsonPrimitive)))
                peers.firstOrNull()?.let { put("enrollment_upstream", it) }
            }.toString(),
        )
        val started = ProcessBuilder(binary.path, config.path).redirectError(ProcessBuilder.Redirect.appendTo(log)).start()
        process = started
        contact = started.inputStream.bufferedReader().readLine() ?: error("rocky-peer exited; see $log")
        check(contact.startsWith("rocky://peer/")) { contact }
    }

    override fun close() {
        process?.let {
            it.destroy()
            if (!it.waitFor(30, TimeUnit.SECONDS)) it.destroyForcibly()
        }
        process = null
    }
}
