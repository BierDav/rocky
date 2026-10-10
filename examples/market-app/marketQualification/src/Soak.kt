package org.example.examples.market.qualification

import androidx.sqlite.driver.bundled.BundledSQLiteDriver
import androidx.sqlite.driver.bundled.SQLITE_OPEN_READONLY
import kotlinx.serialization.json.*
import java.io.File

/** The secure runtime's retained command budget: `MAX_SECURE_RECORDS` and `MAX_SECURE_BYTES`. */
private const val RECORDS = 4_096L
private const val BYTES = 32L * 1024 * 1024

/** Retained command records and bytes of a device, counted as the runtime's budget check counts them. */
private fun usage(device: Device): Pair<Long, Long> = BundledSQLiteDriver().open(File(device.directory, "rocky/rocky.sqlite").path, SQLITE_OPEN_READONLY).use { file ->
    file.prepare(
        "SELECT (SELECT COUNT(*) FROM secure_commands)+(SELECT COUNT(*) FROM secure_recovered_commands)" +
            "+(SELECT COUNT(*) FROM secure_preparations)+(SELECT COUNT(*) FROM secure_results)," +
            "(SELECT COALESCE(SUM(LENGTH(command)),0) FROM secure_commands)+(SELECT COALESCE(SUM(LENGTH(command)),0) FROM secure_recovered_commands)" +
            "+(SELECT COALESCE(SUM(LENGTH(result)),0) FROM secure_preparations)+(SELECT COALESCE(SUM(LENGTH(result)),0) FROM secure_results)",
    ).use {
        it.step()
        it.getLong(0) to it.getLong(1)
    }
}

/**
 * [target] strong commands as JVM processes against three voters: the seller restocks one lamp and a buyer reserves
 * it, alternately, so the command tables stay small. Each window of 256 commands records the latency and both
 * devices' retained records and bytes against their budgets. It stops at the target, or at the first failure, with
 * the number of commands reached and the budget or error that stopped it.
 */
internal fun soak(voter: File, evidence: File, target: Int) {
    val root = File(evidence, "soak").apply {
        deleteRecursively()
        mkdirs()
    }
    val windows = mutableListOf<JsonObject>()
    var reached = 0
    var stopped: String? = null
    val started = System.nanoTime()
    Voters(voter, File(root, "voters")).use { voters ->
        Run(root, voters, File(""), mutableListOf()).use { run ->
            val (seller, buyers) = run.market("buyer")
            val buyer = buyers.single()
            var window = System.nanoTime()
            fun handle(n: Int) = "%064x".format(n + 1_000_000)
            try {
                while (reached < target) {
                    val restocked = seller.ticket("restock", "item" to "lamp", "quantity" to 1, "handle" to handle(reached))
                    expect(restocked.state == "Applied") { "restock ${reached + 1}: $restocked" }
                    reached++
                    if (reached < target) {
                        buyer.ticket("submit", "item" to "lamp", "quantity" to 1, "hold" to "soak", "handle" to handle(reached))
                        val reserved = run.settle(buyer, handle(reached))
                        expect(reserved.outcome == "accepted Remaining(remaining=0)") { "reservation ${reached + 1}: $reserved" }
                        reached++
                    }
                    // At most 32 identities stay outstanding per device until acknowledged.
                    if (reached % 16 == 0) listOf(seller, buyer).forEach { it.call("acknowledge") }
                    if (reached % 256 == 0 || reached == target) {
                        val now = System.nanoTime()
                        val sample = buildJsonObject {
                            put("commands", reached)
                            put("mean_ms", (now - window) / 1_000_000.0 / 256)
                            for (device in listOf(seller, buyer)) {
                                val (records, bytes) = usage(device)
                                put("${device.name}_records", records)
                                put("${device.name}_bytes", bytes)
                                put("${device.name}_evidence_bytes", File(device.directory, "rocky/coordinator.evidence").length())
                            }
                        }
                        windows += sample
                        println("soak $sample")
                        window = now
                    }
                }
            } catch (error: Exception) {
                stopped = error.message
            } catch (error: AssertionError) {
                stopped = error.message
            }
            for (device in listOf(seller, buyer)) {
                val (records, bytes) = usage(device)
                if (records >= RECORDS) stopped = (stopped ?: "") + " [${device.name} at the record budget: $records of $RECORDS]"
                if (bytes >= BYTES * 9 / 10) stopped = (stopped ?: "") + " [${device.name} near the byte budget: $bytes of $BYTES]"
            }
        }
    }
    val result = buildJsonObject {
        put("target", target)
        put("reached", reached)
        put("completed", reached == target && stopped == null)
        put("stopped", stopped)
        put("record_budget", RECORDS)
        put("byte_budget", BYTES)
        put("milliseconds", (System.nanoTime() - started) / 1_000_000)
        environment(3).forEach { (key, value) -> put(key, value) }
        put("windows", JsonArray(windows))
    }
    File(evidence, "soak.json").writeText(result.toString())
    println("${if (reached == target && stopped == null) "PASS" else "STOP"} soak: $reached of $target commands${stopped?.let { ": $it" } ?: ""}")
    println("evidence: ${File(evidence, "soak.json")}")
    // Stopping at a named budget is a result; any other stop is a failure.
    if (stopped != null && listOf("quota", "budget").none { it in stopped.orEmpty() }) kotlin.system.exitProcess(1)
}
