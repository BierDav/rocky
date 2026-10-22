package org.example.examples.shopping.qualification

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonObject
import java.io.File
import java.net.DatagramSocket
import java.net.ServerSocket
import java.util.concurrent.TimeUnit

/**
 * Three loopback voters of the coordination service, provisioned as `scripts/coordinator-local` does. [bundle] is the
 * trust bundle devices pin. `ROCKY_VOTER` names the `rocky-voter` executable.
 */
internal class Voters(private val root: File) : AutoCloseable {
    private val voter = File(System.getenv("ROCKY_VOTER") ?: error("set ROCKY_VOTER to the rocky-voter executable"))
    private val service = File(root, "service")
    private val processes = mutableListOf<Process>()
    val bundle: File = File(service, "service/bundle.json")

    init {
        command(voter.path, "local", service.path, freeBlock().toString())
        for (id in 1..3) {
            processes += ProcessBuilder(voter.path, File(service, "voter-$id.json").path, "--provision")
                .redirectErrorStream(true).redirectOutput(File(root, "voter-$id.log")).start()
        }
        val admin = File(service, "admin.json").path
        val deadline = System.nanoTime() + 60_000_000_000
        while (runCatching { command(voter.path, "admin", admin, "initialize") }.isFailure) {
            check(System.nanoTime() < deadline) { "voters did not come up; see ${root.path}/voter-*.log" }
            Thread.sleep(50)
        }
        while (runCatching { Json.parseToJsonElement(command(voter.path, "admin", admin, "status", "1")).jsonObject["leader"] != JsonNull }.getOrDefault(false).not()) {
            check(System.nanoTime() < deadline) { "voters elected no leader" }
            Thread.sleep(50)
        }
    }

    private fun command(vararg arguments: String): String {
        val output = File(root, "voter-command.log")
        val process = ProcessBuilder(*arguments).redirectErrorStream(true).redirectOutput(output).start()
        check(process.waitFor(60, TimeUnit.SECONDS)) { "voter command timed out" }
        check(process.exitValue() == 0) { output.readText() }
        return output.readText()
    }

    /** A base port whose voter TCP ports (base+1..3) and iroh UDP ports (base+11..13) are free. */
    private fun freeBlock(): Int {
        repeat(32) {
            val base = kotlin.random.Random.nextInt(10_000, 60_000)
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
        error("no free port block")
    }

    override fun close() {
        processes.forEach { it.destroy() }
        processes.forEach { if (!it.waitFor(10, TimeUnit.SECONDS)) it.destroyForcibly() }
    }
}
