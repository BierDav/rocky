package org.example.examples.market.qualification

import kotlinx.serialization.json.*
import java.io.File
import java.net.InetAddress
import java.nio.file.Files
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Multi-process market qualification against three `rocky-voter` processes on loopback. Every device is its own JVM
 * running the app's [org.example.examples.market.MarketNode]; the custody peer is the packaged `rocky-peer`. The
 * application has no Rust source.
 *
 * `ROCKY_VOTER` and `ROCKY_PEER` name the executables, `ROCKY_EVIDENCE` the directory for logs and `evidence.json`,
 * `ROCKY_SCENARIOS` a comma-separated selection and `ROCKY_BUILD` the build the evidence names. `ROCKY_SOAK` runs
 * the soak instead ([soak]). See docs/runtime/market-qualification.md.
 */
fun main() {
    val voter = executable("ROCKY_VOTER")
    val peer = executable("ROCKY_PEER")
    val evidence = File(System.getenv("ROCKY_EVIDENCE") ?: Files.createTempDirectory("market-qualification").toString())
    evidence.mkdirs()
    System.getenv("ROCKY_SOAK")?.let { return soak(voter, evidence, it.toInt()) }
    val only = System.getenv("ROCKY_SCENARIOS")?.split(",")?.map(String::trim)?.toSet()
    only?.let { names -> expect(names.all { name -> scenarios.any { it.first == name } }) { "unknown scenario in $names; known: ${scenarios.map { it.first }}" } }
    val results = mutableListOf<JsonObject>()
    var failed = false
    // One service for the run: each scenario registers its own domain and closes it at the end.
    Voters(voter, File(evidence, "coordination").apply { deleteRecursively() }).use { voters ->
        for ((name, scenario) in scenarios) {
            if (only != null && name !in only) continue
            val root = File(evidence, name).apply {
                deleteRecursively()
                mkdirs()
            }
            val started = System.nanoTime()
            val notes = mutableListOf<String>()
            val error = runCatching { Run(root, voters, peer, notes).use { scenario(it) } }.exceptionOrNull()
            val millis = (System.nanoTime() - started) / 1_000_000
            failed = failed || error != null
            println("${if (error == null) "PASS" else "FAIL"} $name ($millis ms)${error?.let { ": ${it.message}" } ?: ""}")
            notes.forEach { println("  $it") }
            results += buildJsonObject {
                put("scenario", name)
                put("passed", error == null)
                put("milliseconds", millis)
                environment(voters.count).forEach { (key, value) -> put(key, value) }
                put("notes", JsonArray(notes.map(::JsonPrimitive)))
                error?.let { put("error", it.stackTraceToString()) }
            }
        }
    }
    File(evidence, "evidence.json").writeText(JsonArray(results).toString())
    println("evidence: ${File(evidence, "evidence.json")}")
    if (failed) kotlin.system.exitProcess(1)
}

private fun executable(variable: String): File = File(System.getenv(variable) ?: error("set $variable")).also {
    check(it.canExecute()) { "$it is not executable" }
}

/** Where and on what the evidence was produced. */
internal fun environment(voters: Int): Map<String, JsonElement> = mapOf(
    "host" to JsonPrimitive(
        "${runCatching { InetAddress.getLocalHost().hostName }.getOrDefault("unknown")} " +
            "${System.getProperty("os.name")} ${System.getProperty("os.version")} ${System.getProperty("os.arch")}, " +
            "Java ${System.getProperty("java.version")}, ${Runtime.getRuntime().availableProcessors()} cores",
    ),
    "build" to JsonPrimitive(System.getenv("ROCKY_BUILD") ?: "unknown"),
    "voters" to JsonPrimitive(voters),
)

private val scenarios: List<Pair<String, (Run) -> Unit>> = listOf(
    "last-item" to Run::lastItem,
    "mesh" to Run::mesh,
    "loss" to Run::loss,
    "takeover" to Run::takeover,
    "refused-writes" to Run::refusedWrites,
    "voters" to Run::voters,
)

internal class Run(val root: File, val voters: Voters, private val peer: File, val notes: MutableList<String>) : AutoCloseable {
    private val opened = mutableListOf<AutoCloseable>()
    private var owner: Device? = null

    fun device(name: String) = Device(name, root, voters.bundle).also { opened += it }

    /** Closes the scenario's domain, freeing the tenant's quota, then every process. */
    override fun close() {
        owner?.let { runCatching { it.call("closeDomain") }.onFailure { error -> notes += "domain not closed: ${error.message}" } }
        opened.asReversed().forEach { runCatching { it.close() } }
    }

    /**
     * The seller creates the market and registers its domain; each buyer joins with a buyer invitation and publishes
     * its command identity as the app does, and the seller admits the published identities.
     */
    fun market(vararg buyers: String): Pair<Device, List<Device>> {
        val seller = device("seller").open("create")
        owner = seller
        seller.call("control")
        val joined = buyers.map { name ->
            device(name).open("join", "invitation" to seller.text("pairing", "profile" to "buyer")).also {
                it.call("publish")
                it.sync()
                seller.call("addPeer", "contact" to it.contact())
            }
        }
        val control = seller.call("control").jsonObject.getValue("revision").jsonPrimitive.long
        notes += "domain registered; ${buyers.size} published identities admitted at control revision $control"
        seller.call("listing", "key" to "lamp", "title" to "Lamp")
        return seller to joined
    }

    private fun handle(n: Int) = "%064x".format(n)

    /** Runs the same ticket until it settles, through retryable failures only. */
    fun settle(device: Device, handle: String, millis: Long = 60_000): TicketView = retry(millis, "${device.name} settles $handle", pause = 200) {
        val ticket = device.ticket("run", "handle" to handle)
        if (!ticket.resolved) {
            expect(ticket.retryable != false) { "${device.name} $handle failed for good: $ticket" }
            error("unresolved: $ticket")
        }
        ticket
    }

    fun restock(seller: Device, quantity: Long, n: Int) {
        val ticket = seller.ticket("restock", "item" to "lamp", "quantity" to quantity, "handle" to handle(n))
        expect(ticket.state == "Applied") { "restock: $ticket" }
    }

    /** Two buyers reserve the last item at once: one is accepted, the other's typed rejection survives kill and reopen. */
    fun lastItem() {
        val (seller, buyers) = market("b", "c")
        restock(seller, 1, 1)
        buyers.forEach { it.sync() }
        val handles = buyers.withIndex().associate { (index, buyer) -> buyer to handle(100 + index) }
        for ((buyer, handle) in handles) {
            val queued = buyer.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to buyer.name, "handle" to handle)
            expect(queued.state == "Queued") { "${buyer.name} submit: $queued" }
        }
        // Both run at once, each in its own process.
        val pool = Executors.newFixedThreadPool(2)
        val outcomes = try {
            handles.map { (buyer, handle) -> pool.submit<TicketView> { settle(buyer, handle) } }.associate { it.get(120, TimeUnit.SECONDS).let { t -> t.outcome to t } }
        } finally {
            pool.shutdownNow()
        }
        val accepted = "accepted Remaining(remaining=0)"
        val rejected = "rejected Shortfall(remaining=0, requested=1)"
        expect(outcomes.keys == setOf(accepted, rejected)) { "outcomes: ${outcomes.values}" }
        notes += "one reservation $accepted, the other $rejected"

        val loser = handles.entries.first { (buyer, handle) -> buyer.ticket("ticket", "handle" to handle).outcome == rejected }
        loser.key.restart()
        val kept = loser.key.ticket("ticket", "handle" to loser.value)
        expect(kept.outcome == rejected && kept.resolved) { "after kill and reopen: $kept" }
        notes += "${loser.key.name}'s rejection kept across SIGKILL and reopen (${kept.state})"
        for (device in listOf(seller) + buyers) {
            if (device != seller) device.sync()
            expect(device.list("offers") == listOf("lamp=0")) { "${device.name} reads ${device.list("offers")} through Room" }
        }
        expect(seller.calls()["reserve b"] == 1 && seller.calls()["reserve c"] == 1) { "handler calls ${seller.calls()}" }
        notes += "every device reads lamp=0 through the Room DAO; one handler call per reservation"
    }

    /** Buyers enroll and sync through custody alone before requesting the executor's contact. */
    fun mesh() {
        val seller = device("seller").open("create")
        owner = seller
        seller.call("control")
        val custody = Custody(root, peer, seller.text("pairing", "profile" to "custody"), listOf(seller.contact())).also { opened += it }
        val buyers = listOf("b", "c").map { name ->
            val invitation = seller.text("pairing", "profile" to "buyer")
            val routed = seller.text("routeEnrollment", "invitation" to invitation, "contact" to custody.contact)
            device(name).open("join", "invitation" to routed).also {
                it.call("retainOnly", "contact" to custody.contact)
                it.call("publish")
                it.sync()
            }
        }
        retry(30_000, "custody delivers caller identities", pause = 200) {
            seller.sync()
            check(seller.call("published").jsonPrimitive.int >= 3) { "caller identities not yet held" }
            seller.call("control")
        }
        restock(seller, 1, 1)
        val b = buyers[0]
        val c = buyers[1]
        val first = handle(401)
        val second = handle(402)
        b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "mesh-b", "handle" to first)
        c.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "mesh-c", "handle" to second)
        expect(settle(b, first).outcome == "accepted Remaining(remaining=0)") { "neighbor caller acceptance" }
        expect(settle(c, second).outcome == "rejected Shortfall(remaining=0, requested=1)") { "neighbor caller shortfall" }
        expect(seller.calls()["reserve mesh-b"] == 1 && seller.calls()["reserve mesh-c"] == 1) { "handler calls ${seller.calls()}" }
        notes += "buyers enrolled and synced through custody alone; executor discovery produced one acceptance and one typed rejection"
    }

    /** A lost reply, a killed caller and a killed executor: each original ticket completes with one handler call. */
    fun loss() {
        val (seller, buyers) = market("b")
        val b = buyers.single()
        restock(seller, 3, 1)
        b.sync()
        fun submit(n: Int, hold: String) = handle(n).also {
            expect(b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to hold, "handle" to it).state == "Queued") { "submit $hold" }
        }

        // Lost reply: the executor confirms, and its answer never reaches the caller.
        val lost = submit(200, "lost-reply")
        seller.call("execute", "envelope" to b.text("envelope", "handle" to lost))
        expect(b.ticket("ticket", "handle" to lost).state == "OutcomeUnknown") { "lost reply left ${b.ticket("ticket", "handle" to lost)}" }
        val recovered = settle(b, lost)
        expect(recovered.outcome == "accepted Remaining(remaining=2)") { "lost reply: $recovered" }
        notes += "lost reply: the same ticket read the confirmed result ($recovered)"

        // Killed caller: SIGKILL after its invocation left, before the executor confirmed it.
        val killed = submit(201, "killed-caller")
        val envelope = b.text("envelope", "handle" to killed)
        b.kill()
        seller.call("execute", "envelope" to envelope)
        b.open("resume")
        expect(b.ticket("ticket", "handle" to killed).state == "OutcomeUnknown") { "killed caller reopened with ${b.ticket("ticket", "handle" to killed)}" }
        val resumed = settle(b, killed)
        expect(resumed.outcome == "accepted Remaining(remaining=1)") { "killed caller: $resumed" }
        notes += "killed caller: after reopen the same ticket completed ($resumed)"

        // Killed executor: SIGKILL after it confirmed, before its answer left.
        val orphaned = submit(202, "killed-executor")
        seller.call("execute", "envelope" to b.text("envelope", "handle" to orphaned))
        seller.kill()
        val waiting = b.ticket("run", "handle" to orphaned)
        expect(!waiting.resolved && waiting.retryable == true) { "executor down: $waiting" }
        seller.open("resume")
        val completed = settle(b, orphaned)
        expect(completed.outcome == "accepted Remaining(remaining=0)") { "killed executor: $completed" }
        notes += "killed executor: the caller waited with ${waiting.error}, then completed after restart ($completed)"

        val calls = seller.calls()
        for (hold in listOf("lost-reply", "killed-caller", "killed-executor")) {
            expect(calls["reserve $hold"] == 1) { "$hold: ${calls["reserve $hold"]} handler calls" }
        }
        b.sync()
        expect(b.list("holds").toSet() == setOf("killed-caller:lamp=1", "killed-executor:lamp=1", "lost-reply:lamp=1")) { "holds ${b.list("holds")}" }
        notes += "one handler call per identity: $calls"
    }

    /** The owner fences the executor; another executor continues at the same head and the fenced one decides nothing. */
    fun takeover() {
        val (seller, buyers) = market("b")
        val b = buyers.single()
        val backup = device("backup").open("join", "invitation" to seller.text("pairing", "profile" to "issuer"))
        backup.call("publish")
        backup.sync()
        seller.call("addPeer", "contact" to backup.contact())
        b.call("addPeer", "contact" to backup.contact())
        restock(seller, 3, 1)
        b.sync()
        b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "before", "handle" to handle(300))
        expect(settle(b, handle(300)).outcome == "accepted Remaining(remaining=2)") { "before takeover" }
        val head = seller.call("sequence").jsonPrimitive.long
        backup.sync()

        // The owner moves execution to the backup: the seller is fenced, the head is kept.
        val revision = seller.call("control", "executors" to listOf(backup.identity())).jsonObject.getValue("revision").jsonPrimitive.long
        notes += "control revision $revision names the backup as executor at head $head"

        // The fenced seller answers without deciding.
        b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "after", "handle" to handle(301))
        val answer = seller.text("execute", "envelope" to b.text("envelope", "handle" to handle(301)))
        val fenced = b.ticket("settle", "handle" to handle(301), "answer" to answer)
        expect(fenced.outcome == null && seller.calls()["reserve after"] == null) { "the fenced executor decided: $fenced" }
        notes += "the fenced seller answered without deciding; the ticket stayed ${fenced.state}"

        // An idle executor acquires only when it runs a ticket of its own: the backup restocks, at the next sequence.
        val acquired = backup.ticket("restock", "item" to "lamp", "quantity" to 1, "handle" to handle(302))
        expect(acquired.state == "Applied" && acquired.sequence == head + 1) { "backup's first command: $acquired, head was $head" }
        expect(backup.calls()["restock lamp"] == 1) { "backup calls ${backup.calls()}" }
        notes += "the backup acquired the domain with its own restock, confirmed at sequence ${acquired.sequence} after head $head"

        // The caller's same ticket completes at the backup.
        val continued = settle(b, handle(301))
        expect(continued.outcome == "accepted Remaining(remaining=2)" && continued.sequence == head + 2) { "after takeover: $continued" }
        expect(backup.calls()["reserve after"] == 1 && seller.calls()["reserve after"] == null) { "calls: seller ${seller.calls()}, backup ${backup.calls()}" }
        notes += "the caller's original ticket was decided once, by the backup, at sequence ${continued.sequence}"
        seller.sync()
        expect(seller.list("offers") == listOf("lamp=2")) { "seller after takeover: ${seller.list("offers")}" }
    }

    /** DAO and SQL writes to command-controlled rows fail on every device; mergeable writes beside them succeed. */
    fun refusedWrites() {
        val (seller, buyers) = market("b")
        val b = buyers.single()
        restock(seller, 2, 1)
        b.sync()
        val head = seller.call("sequence").jsonPrimitive.long
        for (device in listOf(seller, b)) {
            val dao = device.text("overwriteStock")
            expect(dao.startsWith("refused") && "Error code: 23" in dao) { "${device.name} DAO write: $dao" }
            for (sql in listOf("UPDATE stock SET remaining = 99", "INSERT INTO holds (key, item, quantity, id, row_kind) VALUES ('forged', 'lamp', 1, 99, 0)", "DELETE FROM stock")) {
                val result = device.text("sqlWrite", "sql" to sql)
                expect(result.startsWith("refused")) { "${device.name} $sql: $result" }
            }
        }
        notes += "DAO upserts failed with SQLITE_AUTH (23) and SQL UPDATE, INSERT and DELETE were refused on the seller and a buyer"
        b.call("memo", "listing" to "lamp", "note" to "Mergeable beside it")
        b.sync()
        seller.call("listing", "key" to "desk", "title" to "Desk")
        b.sync()
        for (device in listOf(seller, b)) {
            expect(device.list("offers") == listOf("desk=null", "lamp=2")) { "${device.name}: ${device.list("offers")}" }
            expect(device.call("sequence").jsonPrimitive.long == head) { "${device.name} moved the confirmed head" }
        }
        expect(seller.list("memos") == listOf("Mergeable beside it")) { "seller memos ${seller.list("memos")}" }
        notes += "stock stayed lamp=2 at head $head; a memo and a listing synced both ways"
    }

    /**
     * Leader loss: strong work continues. Quorum loss: it pauses while mergeable writes sync through the custody peer.
     * After restore the pending ticket applies once.
     */
    fun voters() {
        val (seller, buyers) = market("b")
        val b = buyers.single()
        val custody = Custody(root, peer, seller.text("pairing", "profile" to "custody"), listOf(seller.contact(), b.contact())).also { opened += it }
        for (device in listOf(seller, b)) device.call("addPeer", "contact" to custody.contact)
        restock(seller, 3, 1)
        b.sync()

        val leader = voters.leader()
        voters.kill(leader)
        b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "leader-lost", "handle" to handle(400))
        val started = System.nanoTime()
        val afterLeader = settle(b, handle(400), millis = 120_000)
        expect(afterLeader.outcome == "accepted Remaining(remaining=2)") { "after leader loss: $afterLeader" }
        notes += "leader voter $leader killed; strong work continued in ${(System.nanoTime() - started) / 1_000_000} ms, new leader ${voters.leader()}"

        val second = voters.running.first()
        voters.kill(second)
        b.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "quorum-lost", "handle" to handle(401))
        val paused = b.ticket("run", "handle" to handle(401))
        expect(!paused.resolved && paused.retryable == true) { "without quorum: $paused" }
        notes += "quorum lost (voters ${voters.running} of 3 running); the ticket paused ${paused.state} with ${paused.error}"
        b.call("memo", "listing" to "lamp", "note" to "Written without a quorum")
        b.sync()
        seller.sync()
        expect(seller.list("memos") == listOf("Written without a quorum")) { "seller memos ${seller.list("memos")}" }
        notes += "a buyer's memo synced to the seller and the custody peer while strong work paused"

        voters.start(leader)
        voters.start(second)
        val restored = settle(b, handle(401), millis = 120_000)
        expect(restored.outcome == "accepted Remaining(remaining=1)") { "after restore: $restored" }
        expect(seller.calls()["reserve quorum-lost"] == 1) { "handler calls ${seller.calls()}" }
        notes += "after restore the paused ticket applied once (${restored.state}, one handler call), leader ${voters.leader()}"
    }
}
