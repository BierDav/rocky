package org.example.examples.shopping.qualification

import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.serialization.json.*

private const val PACKAGE = "org.example.examples"
private const val TAG = "RockyShopping"

/** One Android device running the shopping app, driven by its intent extras; results are read from logcat. */
internal class Phone(val name: String, val serial: String, private val log: File) {
    private var step = 0
    fun adb(vararg args: String, strict: Boolean = true): String {
        val process = ProcessBuilder(listOf("adb", "-s", serial) + args).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        check(process.waitFor(120, TimeUnit.SECONDS)) { "adb ${args.joinToString(" ")} timed out" }
        if (strict && process.exitValue() != 0) error("adb ${args.joinToString(" ")}: $output")
        return output
    }
    fun property(name: String) = adb("shell", "getprop", name).trim()
    /** Retried writes: each timed out (code 4) while background sync held the replica, and was rolled back. */
    val retries = mutableListOf<String>()
    /** Sends one intent and waits for its `done` line; returns the app's log lines for it. */
    fun send(vararg extras: Pair<String, String>, timeoutSeconds: Long = 120): List<String> {
        repeat(3) {
            try { return attempt(extras, timeoutSeconds) } catch (busy: Busy) { retries += "${extras.toList()}: ${busy.message}" }
        }
        return attempt(extras, timeoutSeconds)
    }
    private class Busy(message: String) : Exception(message)
    private fun attempt(extras: Array<out Pair<String, String>>, timeoutSeconds: Long): List<String> {
        val marker = "${name}-${++step}"
        adb("logcat", "-c")
        // `adb shell` passes one command line to the device shell: quote every value.
        val arguments = (extras.toList() + ("step" to marker)).flatMap { (key, value) -> listOf("--es", key, "'$value'") }
        adb("shell", "am", "start", "-n", "$PACKAGE/.MainActivity", *arguments.toTypedArray())
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(timeoutSeconds)
        while (System.nanoTime() < deadline) {
            val lines = adb("logcat", "-d", "-v", "raw", "-s", "$TAG:I").lines()
            lines.firstOrNull { it.startsWith("failed $marker") }?.let {
                if ("Error code: 4," in it) throw Busy(it)
                error("$name: $it")
            }
            if (lines.any { it == "done $marker" }) {
                log.appendText(lines.joinToString("\n", postfix = "\n"))
                return lines
            }
            Thread.sleep(250)
        }
        error("$name: no reply to ${extras.toList()} within $timeoutSeconds s")
    }
    /** `list title -> items`, each `title` or `title [x]`. */
    fun dump(): Map<String, List<String>> = send("dump" to "1").filter { it.startsWith("dump ") }.associate { line ->
        val (title, items) = line.removePrefix("dump ").split(": ", limit = 2)
        title to items.split(",").filter { it.isNotEmpty() }.sorted()
    }
}

/**
 * The release gate's sharing and move steps on two Android devices with a host issuer and the packaged custody
 * peer. `ROCKY_PHONES` names two adb serials and `ROCKY_APK` the app; both reach the host through `adb reverse`.
 */
internal fun phones(peer: File, evidence: File) {
    val serials = System.getenv("ROCKY_PHONES").split(",").map { it.trim() }
    require(serials.size == 2) { "ROCKY_PHONES names two adb serials" }
    val apk = File(System.getenv("ROCKY_APK") ?: error("set ROCKY_APK to the shopping app's APK"))
    val root = File(evidence, "phones").apply { deleteRecursively(); mkdirs() }
    val notes = mutableListOf<String>()
    val started = System.nanoTime()
    fun elapsed() = (System.nanoTime() - started) / 1_000_000
    val (a, b) = serials.mapIndexed { index, serial -> Phone("phone-${'a' + index}", serial, File(root, "phone-${'a' + index}.log")) }
    val devices = listOf(a, b).associate { phone ->
        phone.name to buildJsonObject {
            put("serial", phone.serial)
            put("model", phone.property("ro.product.model"))
            put("sdk", phone.property("ro.build.version.sdk"))
            put("abi", phone.property("ro.product.cpu.abi"))
        }
    }
    val error = runCatching {
        Run(root, peer, notes).use { run ->
            val host = run.device("host").open("create")
            val hostPort = host.listen.substringAfterLast(':')
            for (phone in listOf(a, b)) {
                phone.adb("install", "-r", apk.path)
                phone.adb("shell", "pm", "clear", PACKAGE)
                phone.adb("shell", "pm", "grant", PACKAGE, "android.permission.ACCESS_LOCAL_NETWORK", strict = false)
                phone.adb("reverse", "tcp:$hostPort", "tcp:$hostPort")
                // Each phone is an issuing device of the user, so it assigns its own authors and shares lists.
                phone.send("invitation" to host.call("pairing", "profile" to "issuer").jsonPrimitive.content, "transport" to "tcp")
            }
            val contacts = listOf(a, b).associateWith { phone ->
                phone.send("contact" to "1").first { it.startsWith("contact ") }.split(" ").let { it[1] to it[2] }
            }
            val custody = run.custody(host.call("pairing", "profile" to "custody").jsonPrimitive.content, host)
            // The custody peer serves configured contacts only: restart it with the phones'.
            val config = File(root, "peer.json")
            val stored = Json.parseToJsonElement(config.readText()).jsonObject
            config.writeText(JsonObject(stored + ("peers" to JsonArray((contacts.values.map { it.second } + host.contact()).map(::JsonPrimitive)))).toString())
            custody.restart()
            val custodyPort = custody.listen.substringAfterLast(':')
            for (phone in listOf(a, b)) {
                phone.adb("reverse", "tcp:$custodyPort", "tcp:$custodyPort")
                phone.send("peer" to custody.contact)
            }
            host.close()
            notes += "${elapsed()} ms: both devices enrolled; the host issuer is closed"

            a.send("add" to "Milk", "list" to "Weekly")
            a.send("add" to "Eggs", "list" to "Weekend")
            val bNode = contacts.getValue(b).first
            for (list in listOf("Weekly", "Weekend")) {
                // B was enrolled by the host, so A names it; the Share button lists only A's own enrollments.
                val code = a.send("share" to list, "to" to bNode).first { it.startsWith("share $bNode ") }.substringAfterLast(' ')
                b.send("accept" to code)
            }
            fun relay(from: Phone, to: Phone) {
                to.send("online" to "false")
                from.send("online" to "true", "sync" to "1")
                from.send("online" to "false")
                to.send("online" to "true", "sync" to "1")
            }
            relay(a, b)
            val shared = mapOf("Weekend" to listOf("Eggs"), "Weekly" to listOf("Milk"))
            check(b.dump() == shared) { "B: ${b.dump()} ${b.send("status" to "1").filter { it.startsWith("status ") }}" }
            notes += "${elapsed()} ms: B received both lists through custody only"

            b.send("check" to "Milk", "add" to "Bread", "list" to "Weekly", "sync" to "1")
            b.send("online" to "false")
            a.send("online" to "true")
            a.send("add" to "Bread", "list" to "Weekly")
            a.send("move" to "Milk", "to" to "Weekend")
            relay(a, b)
            relay(b, a)
            val converged = mapOf("Weekend" to listOf("Eggs", "Milk [x]"), "Weekly" to listOf("Bread"))
            check(a.dump() == converged) { "A: ${a.dump()}" }
            check(b.dump() == converged) { "B: ${b.dump()}" }
            notes += "${elapsed()} ms: one Bread survived; the moved Milk kept B's offline tick on both devices"
        }
    }.exceptionOrNull()
    val result = buildJsonObject {
        put("scenario", "phones"); put("passed", error == null); put("milliseconds", elapsed())
        put("devices", JsonObject(devices)); put("notes", JsonArray(notes.map(::JsonPrimitive)))
        put("retried", JsonArray((a.retries + b.retries).map(::JsonPrimitive)))
        error?.let { put("error", it.stackTraceToString()) }
    }
    File(evidence, "phones.json").writeText(result.toString())
    println("${if (error == null) "PASS" else "FAIL"} phones (${elapsed()} ms)${error?.let { ": ${it.message}" } ?: ""}")
    notes.forEach { println("  $it") }
    (a.retries + b.retries).forEach { println("  retried $it") }
    devices.forEach { (name, device) -> println("  $name: $device") }
    if (error != null) kotlin.system.exitProcess(1)
}
