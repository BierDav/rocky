package org.example.examples.shopping.qualification

import androidx.room.Room
import kotlinx.serialization.json.*
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.qualification.LiveLinkAgent
import org.example.examples.shopping.data.qualification.epochMicros
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.PeerTransport
import java.io.Closeable
import java.io.File
import java.io.PrintWriter
import java.lang.management.ManagementFactory
import java.net.Socket
import java.util.concurrent.TimeUnit
import kotlin.math.ceil

/**
 * The phase 6 live-link qualification, as `live-links <command>`: `agent <port>` serves a desktop device; every other
 * command drives agents already serving on loopback ports, the desktop's and the phone's through `adb forward`.
 * Evidence goes to `$ROCKY_EVIDENCE`. See docs/features_08/06-qualification-evidence.md.
 */
internal fun liveLinks(args: List<String>) {
    if (args.firstOrNull() == "agent") return desktopAgent(args[1].toInt())
    LiveLinkDriver().run(args)
}

/**
 * `ROCKY_IROH_BIND` binds the desktop's iroh socket, for instance to loopback so that only the relay carries the link,
 * or to one interface so that an overlay VPN cannot.
 */
private fun desktopAgent(port: Int) {
    val pid = ProcessHandle.current().pid()
    val bind = System.getenv("ROCKY_IROH_BIND")
    LiveLinkAgent(
        platform = { directory, name, _ ->
            ShoppingPlatform(directory, name, room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) }, transport = { PeerTransport.Iroh(bind = bind) })
        },
        log = { System.err.println("${epochMicros()} $it") },
        traffic = { nettop(pid) },
    ).serve(port)
}

/** The process's received and sent bytes, summed over its sockets, as macOS `nettop` reports them. */
private fun nettop(pid: Long): JsonObject {
    val at = epochMicros()
    val lines = command("nettop", "-P", "-L", "1", "-x", "-p", pid.toString(), "-J", "bytes_in,bytes_out").lines()
    val row = lines.drop(1).firstOrNull { it.isNotBlank() }?.split(',')
    return buildJsonObject {
        put("at", at)
        put("rxBytes", row?.getOrNull(1)?.toLongOrNull() ?: 0)
        put("txBytes", row?.getOrNull(2)?.toLongOrNull() ?: 0)
    }
}

private fun command(vararg command: String): String {
    val process = ProcessBuilder(*command).redirectErrorStream(true).start()
    val output = process.inputStream.bufferedReader().readText()
    check(process.waitFor(120, TimeUnit.SECONDS)) { "${command.joinToString(" ")} timed out" }
    return output
}

/** One live-link agent's JSON-line connection. */
private class LiveLinkRemote(val name: String, port: Int) : Closeable {
    private val socket = Socket("127.0.0.1", port).apply { soTimeout = 20 * 60 * 1000 }
    private val reader = socket.getInputStream().bufferedReader()
    private val writer = PrintWriter(socket.getOutputStream(), true)

    fun call(op: String, build: JsonObjectBuilder.() -> Unit = {}): JsonElement {
        writer.println(buildJsonObject { put("op", op); build() })
        val reply = Json.parseToJsonElement(reader.readLine() ?: error("$name closed the connection")).jsonObject
        reply["error"]?.let { error("$name $op: ${it.jsonPrimitive.content}") }
        return reply.getValue("ok")
    }

    /** This agent's clock minus the driver's, from the exchange with the shortest round trip. */
    fun offset(samples: Int = 40): JsonObject {
        val best = (1..samples).map {
            val sent = epochMicros()
            val remote = call("now").jsonPrimitive.long
            val received = epochMicros()
            received - sent to remote - (sent + received) / 2
        }.minBy { it.first }
        return buildJsonObject {
            put("offsetUs", best.second)
            put("roundTripUs", best.first)
        }
    }

    override fun close() = socket.close()
}

private fun JsonElement.obj() = jsonObject
private fun JsonObject.long(name: String) = getValue(name).jsonPrimitive.long
private fun JsonObject.text(name: String) = getValue(name).jsonPrimitive.content

private class LiveLinkDriver {
    private val evidence = File(System.getenv("ROCKY_EVIDENCE") ?: "local/live-links").absoluteFile.also { it.mkdirs() }
    private val serial = System.getenv("ROCKY_PHONE")
    private val desktopPort = System.getenv("ROCKY_DESKTOP_PORT")?.toInt() ?: 47120
    private val phonePort = System.getenv("ROCKY_PHONE_PORT")?.toInt() ?: 47110
    private val stateFile = File(evidence, "pair.json")

    /** The phone's transport options, such as `{"lookup":"disabled","topic":false,"local":false}`. */
    private val phoneTransport = System.getenv("ROCKY_PHONE_TRANSPORT")?.let { Json.parseToJsonElement(it).obj() } ?: JsonObject(emptyMap())

    private fun adb(vararg args: String) = command(*(listOfNotNull("adb", serial?.let { "-s" }, serial) + args).toTypedArray())
    private fun desktop() = LiveLinkRemote("desktop", desktopPort)
    private fun phone(): LiveLinkRemote {
        adb("forward", "tcp:$phonePort", "tcp:${PHONE_AGENT_PORT}")
        return LiveLinkRemote("phone", phonePort)
    }
    private fun state() = Json.parseToJsonElement(stateFile.readText()).obj()

    private fun write(name: String, value: JsonElement) {
        File(evidence, name).writeText(Json { prettyPrint = true }.encodeToString(JsonElement.serializer(), value))
        println("wrote ${File(evidence, name)}")
    }

    fun run(args: List<String>) {
        when (args.first()) {
            "pair" -> pair(args[1], args[2])
            "resume" -> resume(args[1], args[2])
            "prepare" -> prepare()
            "finish" -> finish()
            "status" -> desktop().use { d -> phone().use { p -> println(buildJsonObject { put("desktop", d.call("status")); put("phone", p.call("status")) }) } }
            "latency" -> latency(args[1].toInt(), args.getOrElse(2) { "run" })
            "lifecycle" -> lifecycle(args[1].toInt())
            "idle" -> idle(args[1].toLong())
            "mesh" -> mesh(args[1])
            "undialable" -> undialable(args[1].toInt())
            else -> error("unknown command ${args.first()}")
        }
    }

    /** Enables the agent in the installed debug app and starts it in the foreground. */
    private fun prepare() {
        adb("shell", "run-as", PHONE_PACKAGE, "touch", "files/$PHONE_AGENT_FLAG")
        adb("shell", "am", "force-stop", PHONE_PACKAGE)
        // A start that races the stop is delivered to the dying activity and leaves no process.
        val deadline = System.currentTimeMillis() + 30_000
        while (adb("shell", "pidof", PHONE_PACKAGE).isNotBlank()) {
            check(System.currentTimeMillis() < deadline) { "$PHONE_PACKAGE did not stop" }
            Thread.sleep(200)
        }
        adb("shell", "am", "start", "-W", "-n", PHONE_ACTIVITY)
        while (true) {
            val reached = runCatching { phone().use { it.call("now") } }
            if (reached.isSuccess) return println("phone agent serving")
            check(System.currentTimeMillis() < deadline) { "the phone agent did not start: ${reached.exceptionOrNull()}" }
            Thread.sleep(500)
        }
    }

    /** Disables the agent again; its households stay under the app's `files/live-links`. */
    private fun finish() {
        adb("shell", "run-as", PHONE_PACKAGE, "rm", "-f", "files/$PHONE_AGENT_FLAG")
        adb("shell", "am", "force-stop", PHONE_PACKAGE)
        adb("forward", "--remove", "tcp:$phonePort")
    }

    /** A new household on the desktop; the phone joins it as a full device and receives one list. */
    private fun pair(desktopDirectory: String, phoneDirectory: String) = desktop().use { desktop ->
        phone().use { phone ->
            val d = desktop.call("create") { put("directory", desktopDirectory); put("name", "Desktop") }.obj()
            val code = desktop.call("invite") { put("kind", "Full"); put("name", "Phone") }.jsonPrimitive.content
            val p = phone.call("join") { put("directory", phoneDirectory); put("name", "Phone"); put("code", code); put("transport", phoneTransport) }.obj()
            val list = desktop.call("createList") { put("title", "Qualification") }.jsonPrimitive.long
            val arrived = awaitList(phone, list, 180_000)
            check(arrived) { "the list did not reach the phone" }
            val state = buildJsonObject {
                put("desktopNode", d.text("node"))
                put("phoneNode", p.text("node"))
                put("list", list)
                put("desktopDirectory", desktopDirectory)
                put("phoneDirectory", phoneDirectory)
            }
            stateFile.writeText(state.toString())
            watchAndLink(desktop, phone, state)
            println(state)
        }
    }

    private fun resume(desktopDirectory: String, phoneDirectory: String) = desktop().use { desktop ->
        phone().use { phone ->
            desktop.call("resume") { put("directory", desktopDirectory); put("name", "Desktop") }
            phone.call("resume") { put("directory", phoneDirectory); put("name", "Phone"); put("transport", phoneTransport) }
            watchAndLink(desktop, phone, state())
        }
    }

    private fun awaitList(remote: LiveLinkRemote, list: Long, timeoutMs: Long): Boolean {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (remote.call("lists").obj().containsKey(list.toString())) return true
            Thread.sleep(250)
        }
        return false
    }

    private fun watchAndLink(desktop: LiveLinkRemote, phone: LiveLinkRemote, state: JsonObject) {
        val list = state.long("list")
        desktop.call("watch") { put("list", list) }
        phone.call("watch") { put("list", list) }
        val d = desktop.call("awaitLink") { put("peer", state.text("phoneNode")); put("state", "Open"); put("timeoutMs", 120_000) }
        val p = phone.call("awaitLink") { put("peer", state.text("desktopNode")); put("state", "Open"); put("timeoutMs", 120_000) }
        println("links desktop=$d phone=$p")
    }

    /** [count] edits each way, one at a time: commit on one side, then the other side's observer must fire. */
    private fun latency(count: Int, label: String) = desktop().use { desktop ->
        phone().use { phone ->
            val state = state()
            val list = state.long("list")
            val before = snapshot(desktop, phone)
            val offsetsBefore = offsets(desktop, phone)
            val toPhone = direction(desktop, phone, list, "$label-d", count, offsetsBefore)
            val toDesktop = direction(phone, desktop, list, "$label-p", count, offsetsBefore)
            val offsetsAfter = offsets(desktop, phone)
            val after = snapshot(desktop, phone)
            write(
                "latency-$label.json",
                buildJsonObject {
                    put("offsetsBefore", offsetsBefore)
                    put("offsetsAfter", offsetsAfter)
                    put("desktopToPhone", toPhone)
                    put("phoneToDesktop", toDesktop)
                    put("before", before)
                    put("after", after)
                },
            )
            println("desktop->phone ${toPhone["summary"]}")
            println("phone->desktop ${toDesktop["summary"]}")
        }
    }

    private fun offsets(desktop: LiveLinkRemote, phone: LiveLinkRemote) = buildJsonObject {
        put("desktop", desktop.offset())
        put("phone", phone.offset())
    }

    private fun offsetOf(offsets: JsonObject, remote: LiveLinkRemote) = offsets.getValue(remote.name).obj().long("offsetUs")

    private fun direction(from: LiveLinkRemote, to: LiveLinkRemote, list: Long, prefix: String, count: Int, offsets: JsonObject): JsonObject {
        val samples = mutableListOf<JsonObject>()
        val latencies = mutableListOf<Double>()
        var missing = 0
        for (i in 1..count) {
            val title = "$prefix-$i"
            val added = from.call("add") { put("list", list); put("title", title) }.obj()
            val seen = to.call("awaitSeen") {
                put("titles", JsonArray(listOf(JsonPrimitive(title))))
                put("timeoutMs", 60_000)
            }.obj()[title]?.jsonPrimitive?.long
            val committed = added.long("committed") - offsetOf(offsets, from)
            val observed = seen?.let { it - offsetOf(offsets, to) }
            val latencyMs = observed?.let { (it - committed) / 1000.0 }
            if (latencyMs == null) missing++ else latencies += latencyMs
            samples += buildJsonObject {
                put("title", title)
                put("commitMs", (added.long("committed") - added.long("start")) / 1000.0)
                put("latencyMs", latencyMs)
            }
            Thread.sleep(1_000)
        }
        return buildJsonObject {
            put("summary", summary(latencies, missing))
            put("samples", JsonArray(samples))
        }
    }

    private fun summary(values: List<Double>, missing: Int): JsonObject {
        val sorted = values.sorted()
        fun rank(p: Double) = sorted.getOrNull((ceil(p * sorted.size).toInt() - 1).coerceAtLeast(0))
        return buildJsonObject {
            put("count", sorted.size)
            put("missing", missing)
            put("medianMs", if (sorted.isEmpty()) null else (sorted[(sorted.size - 1) / 2] + sorted[sorted.size / 2]) / 2)
            put("p95Ms", rank(0.95))
            put("minMs", sorted.firstOrNull())
            put("maxMs", sorted.lastOrNull())
        }
    }

    private fun snapshot(desktop: LiveLinkRemote, phone: LiveLinkRemote) = buildJsonObject {
        put("desktopStatus", desktop.call("status"))
        put("phoneStatus", phone.call("status"))
        put("desktopTraffic", desktop.call("traffic"))
        put("phoneTraffic", phone.call("traffic"))
    }

    /**
     * The phone goes home and comes back: the desktop sees its link close, commits [edits] edits while it is away,
     * and the phone must observe them after returning.
     */
    private fun lifecycle(edits: Int) = desktop().use { desktop ->
        val state = state()
        val list = state.long("list")
        val phoneNode = state.text("phoneNode")
        val desktopNode = state.text("desktopNode")
        val (phoneLinks, phoneEvents, offset) = phone().use { phone ->
            check(phone.call("awaitLink") { put("peer", desktopNode); put("state", "Open"); put("timeoutMs", 60_000) }.obj().getValue("reached").jsonPrimitive.boolean)
            Triple(phone.call("links").jsonArray.size, phone.call("events").jsonArray.size, phone.offset())
        }
        val home = epochMicros()
        adb("shell", "input", "keyevent", "KEYCODE_HOME")
        val closed = desktop.call("awaitLink") { put("peer", phoneNode); put("state", "NotOpen"); put("timeoutMs", 120_000) }.obj()
        val titles = (1..edits).map { "background-$home-$it" }
        val committed = titles.map { title -> desktop.call("add") { put("list", list); put("title", title) }.obj().long("committed") }
        Thread.sleep(30_000)
        val stillClosed = desktop.call("status").obj().getValue("peers").jsonArray.map { it.obj() }.first { it.text("node") == phoneNode }.text("link")
        val start = epochMicros()
        adb("shell", "am", "start", "-n", PHONE_ACTIVITY)
        phone().use { phone ->
            val seen = phone.call("awaitSeen") { put("titles", JsonArray(titles.map(::JsonPrimitive))); put("timeoutMs", 120_000) }.obj()
            val reopened = desktop.call("awaitLink") { put("peer", phoneNode); put("state", "Open"); put("timeoutMs", 120_000) }.obj()
            Thread.sleep(5_000)
            val phoneOffset = offset.long("offsetUs")
            write(
                "lifecycle.json",
                buildJsonObject {
                    put("homePressedUs", home)
                    put("desktopSawCloseAfterMs", (closed.long("at") - home) / 1000.0)
                    put("desktopCloseReached", closed.getValue("reached"))
                    put("committedUs", JsonArray(committed.map(::JsonPrimitive)))
                    put("desktopLinkBeforeForeground", stillClosed)
                    put("foregroundStartedUs", start)
                    put("desktopSawReopenAfterMs", (reopened.long("at") - start) / 1000.0)
                    put("seenCount", seen.size)
                    put("lastSeenAfterForegroundMs", seen.values.maxOfOrNull { (it.jsonPrimitive.long - phoneOffset - start) / 1000.0 })
                    put("phoneOffset", offset)
                    put("phoneLinks", JsonArray(phone.call("links").jsonArray.drop(phoneLinks)))
                    put("phoneEvents", JsonArray(phone.call("events").jsonArray.drop(phoneEvents)))
                    put("desktopStatus", desktop.call("status"))
                },
            )
        }
    }

    /** [minutes] without edits: counters, revisions and link transitions at both ends. */
    private fun idle(minutes: Long) = desktop().use { desktop ->
        phone().use { phone ->
            val dLinks = desktop.call("links").jsonArray.size
            val pLinks = phone.call("links").jsonArray.size
            val dEvents = desktop.call("events").jsonArray.size
            val pEvents = phone.call("events").jsonArray.size
            // Battery stats count radio wake-ups only on battery; USB power is reported as charging until reset.
            // The app does not keep the screen on, and a dark screen would send it to the background.
            val stayOn = adb("shell", "settings", "get", "global", "stay_on_while_plugged_in").trim()
            adb("shell", "svc", "power", "stayon", "true")
            adb("shell", "dumpsys", "battery", "unplug")
            val after = try {
                Thread.sleep(5_000)
                File(evidence, "idle-batterystats-before.txt").writeText(adb("shell", "dumpsys", "batterystats"))
                File(evidence, "idle-netstats-before.txt").writeText(netstats())
                val before = snapshot(desktop, phone)
                Thread.sleep(minutes * 60_000)
                val after = snapshot(desktop, phone)
                File(evidence, "idle-netstats-after.txt").writeText(netstats())
                File(evidence, "idle-batterystats-after.txt").writeText(adb("shell", "dumpsys", "batterystats"))
                before to after
            } finally {
                adb("shell", "dumpsys", "battery", "reset")
                adb("shell", "settings", "put", "global", "stay_on_while_plugged_in", stayOn)
            }
            val (before, afterSnapshot) = after
            write(
                "idle-$minutes.json",
                buildJsonObject {
                    put("before", before)
                    put("after", afterSnapshot)
                    put("desktopLinks", JsonArray(desktop.call("links").jsonArray.drop(dLinks)))
                    put("phoneLinks", JsonArray(phone.call("links").jsonArray.drop(pLinks)))
                    put("desktopEvents", JsonArray(desktop.call("events").jsonArray.drop(dEvents)))
                    put("phoneEvents", JsonArray(phone.call("events").jsonArray.drop(pEvents)))
                },
            )
        }
    }

    private fun netstats(): String {
        adb("shell", "dumpsys", "netstats", "--poll")
        return adb("shell", "dumpsys", "netstats", "detail")
    }

    /**
     * The desktop forgets the phone's route, so only the phone can open their link. The phone leaves and returns:
     * the desktop must not dial while it is away, and after the phone dialed, [count] desktop edits must reach it.
     */
    private fun undialable(count: Int) = desktop().use { desktop ->
        val state = state()
        val list = state.long("list")
        val phoneNode = state.text("phoneNode")
        desktop.call("removePeer") { put("node", phoneNode) }
        val forgotten = desktop.call("status")
        val desktopLinks = desktop.call("links").jsonArray.size
        val desktopEvents = desktop.call("events").jsonArray.size
        val home = epochMicros()
        adb("shell", "input", "keyevent", "KEYCODE_HOME")
        val closed = desktop.call("awaitLink") { put("peer", phoneNode); put("state", "NotOpen"); put("timeoutMs", 120_000) }.obj()
        // While the phone is away, the desktop must not try to open a link to it.
        val away = (1..60).map {
            Thread.sleep(1_000)
            desktop.call("status").obj().getValue("peers").jsonArray.map { it.obj() }.firstOrNull { it.text("node") == phoneNode }?.text("link") ?: "absent"
        }
        val foreground = epochMicros()
        adb("shell", "am", "start", "-n", PHONE_ACTIVITY)
        val reopened = desktop.call("awaitLink") { put("peer", phoneNode); put("state", "Open"); put("timeoutMs", 180_000) }.obj()
        phone().use { phone ->
            val offsets = offsets(desktop, phone)
            val toPhone = direction(desktop, phone, list, "undialable-d", count, offsets)
            write(
                "undialable.json",
                buildJsonObject {
                    put("desktopAfterForgetting", forgotten)
                    put("desktopSawCloseAfterMs", (closed.long("at") - home) / 1000.0)
                    put("desktopLinkWhileAway", JsonArray(away.map(::JsonPrimitive)))
                    put("desktopSawOpenAfterForegroundMs", (reopened.long("at") - foreground) / 1000.0)
                    put("offsets", offsets)
                    put("desktopToPhone", toPhone)
                    put("desktopLinks", JsonArray(desktop.call("links").jsonArray.drop(desktopLinks)))
                    put("desktopEvents", JsonArray(desktop.call("events").jsonArray.drop(desktopEvents)))
                    put("desktopStatus", desktop.call("status"))
                    put("phoneStatus", phone.call("status"))
                },
            )
            println("undialable desktop->phone ${toPhone["summary"]}; while away ${away.toSet()}")
        }
    }

    /**
     * Three desktop processes, each with its own household directory under [root]: every pair must hold a link.
     * One is killed without closing; the others must close its links, and its restart must reopen them.
     */
    private fun mesh(root: String) {
        val ports = listOf(47131, 47132, 47133)
        val names = listOf("A", "B", "C")
        val processes = ports.map(::spawn).toMutableList()
        val remotes = ports.mapIndexed { i, port -> connect(names[i], port) }.toMutableList()
        try {
            val nodes = mutableListOf<String>()
            nodes += remotes[0].call("create") { put("directory", "$root/A"); put("name", "A") }.obj().text("node")
            for (i in 1..2) {
                val code = remotes[0].call("invite") { put("kind", "Full"); put("name", names[i]) }.jsonPrimitive.content
                nodes += remotes[i].call("join") { put("directory", "$root/${names[i]}"); put("name", names[i]); put("code", code) }.obj().text("node")
            }
            val list = remotes[0].call("createList") { put("title", "Mesh") }.jsonPrimitive.long
            check(awaitList(remotes[1], list, 120_000) && awaitList(remotes[2], list, 120_000)) { "the list did not reach every desktop" }
            // B and C learn each other through contact handover; an edit makes every pair reconcile.
            remotes[1].call("add") { put("list", list); put("title", "from-B") }
            remotes[2].call("add") { put("list", list); put("title", "from-C") }
            val open = buildJsonObject {
                for (i in 0..2) for (j in 0..2) if (i != j) {
                    put("${names[i]}->${names[j]}", remotes[i].call("awaitLink") { put("peer", nodes[j]); put("state", "Open"); put("timeoutMs", 180_000) })
                }
            }
            println("mesh open $open")
            val statuses = buildJsonObject { for (i in 0..2) put(names[i], remotes[i].call("status")) }
            remotes[2].close()
            val killed = epochMicros()
            processes[2].destroyForcibly().waitFor()
            val closed = buildJsonObject {
                for (i in 0..1) {
                    val reached = remotes[i].call("awaitLink") { put("peer", nodes[2]); put("state", "NotOpen"); put("timeoutMs", 180_000) }.obj()
                    put(names[i], buildJsonObject { put("reached", reached.getValue("reached")); put("afterMs", (reached.long("at") - killed) / 1000.0) })
                }
            }
            println("mesh closed $closed")
            val restarted = epochMicros()
            processes[2] = spawn(ports[2])
            remotes[2] = connect("C", ports[2])
            remotes[2].call("resume") { put("directory", "$root/C"); put("name", "C") }
            val reopened = buildJsonObject {
                for (i in 0..1) {
                    val reached = remotes[i].call("awaitLink") { put("peer", nodes[2]); put("state", "Open"); put("timeoutMs", 180_000) }.obj()
                    put(names[i], buildJsonObject { put("reached", reached.getValue("reached")); put("afterMs", (reached.long("at") - restarted) / 1000.0) })
                }
            }
            println("mesh reopened $reopened")
            write(
                "mesh.json",
                buildJsonObject {
                    put("nodes", JsonArray(nodes.map(::JsonPrimitive)))
                    put("killedUs", killed)
                    put("restartedUs", restarted)
                    put("open", open)
                    put("statusesBeforeKill", statuses)
                    put("closedAfterKill", closed)
                    put("reopenedAfterRestart", reopened)
                    put("eventsA", remotes[0].call("events"))
                    put("eventsB", remotes[1].call("events"))
                    put("statusesAfter", buildJsonObject { for (i in 0..2) put(names[i], remotes[i].call("status")) })
                },
            )
        } finally {
            remotes.forEach { runCatching { it.call("close") }; it.close() }
            processes.forEach { it.destroy(); it.waitFor(30, TimeUnit.SECONDS); it.destroyForcibly() }
        }
    }

    private fun spawn(port: Int): Process = ProcessBuilder(
        listOf(ProcessHandle.current().info().command().get()) +
            ManagementFactory.getRuntimeMXBean().inputArguments.filterNot { it.startsWith("-agentlib") || it.startsWith("-javaagent") } +
            listOf("-cp", System.getProperty("java.class.path"), "org.example.examples.shopping.qualification.QualificationKt", "live-links", "agent", port.toString()),
    ).redirectErrorStream(true).redirectOutput(ProcessBuilder.Redirect.appendTo(File(evidence, "mesh-$port.log"))).start()

    private fun connect(name: String, port: Int): LiveLinkRemote {
        val deadline = System.currentTimeMillis() + 60_000
        while (true) {
            try {
                return LiveLinkRemote(name, port)
            } catch (error: java.net.ConnectException) {
                if (System.currentTimeMillis() > deadline) throw error
                Thread.sleep(200)
            }
        }
    }

    companion object {
        const val PHONE_AGENT_PORT = 47110
        const val PHONE_PACKAGE = "org.example.examples"
        const val PHONE_ACTIVITY = "$PHONE_PACKAGE/.MainActivity"
        const val PHONE_AGENT_FLAG = "live-links-agent"
    }
}
