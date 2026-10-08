package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*
import java.io.File

suspend fun expectError(code: String? = null, block: suspend () -> Unit) {
    try { block(); error("expected error") } catch (error: RockyException) { check(code == null || error.code == code) }
}
fun JsonObject.count(): Int = getValue("rows").jsonArray.single().jsonArray.single().jsonObject.getValue("value").jsonPrimitive.content.toInt()

fun main(args: Array<String>) = runBlocking {
    withTimeout(60_000) {
        val directory = File(args[0]).apply { mkdirs() }
        val host = File(args[1], "target/debug/examples/sdk_host")
        val database = File(directory, "inventory.db")
        check(!database.exists()) { "qualification requires a fresh directory" }
        var client = RockyClient(ProcessTransport.open(host, database))
        var inventory = InventoryClient(client)
        val all = query("SELECT note FROM drafts ORDER BY note")
        // Registration happens before the first snapshot; first() cancels and releases.
        check(client.observe(all).first().getValue("rows").jsonArray.isEmpty())
        val first = inventory.note(handle(1), "first", baseline = "0")
        expectError { inventory.note(handle(2), "stale", baseline = "0") }
        expectError("conflict") { inventory.note(handle(1), "different") }
        expectError { client.read(query("SELECT * FROM rocky_operations")) }
        check(client.recoverMutation(handle(1)).getValue("state").jsonPrimitive.content == "committed")
        val counter = query("SELECT COUNT(*) FROM drafts")
        val initial = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val newest = CompletableDeferred<Unit>()
        val collector = launch {
            client.observe(counter).collect {
                val count = it.count()
                if (count == 1) { initial.complete(Unit); release.await() }
                if (count == 21) newest.complete(Unit)
            }
        }
        initial.await()
        for (index in 2..21) inventory.note(handle(index + 10), "note-$index")
        release.complete(Unit)
        newest.await()
        collector.cancelAndJoin()
        // Cancel a collector waiting for its next notification.
        val ready = CompletableDeferred<Unit>()
        val waiting = launch { client.observe(counter).collect { ready.complete(Unit) } }
        ready.await()
        waiting.cancelAndJoin()
        // More registrations than the quota prove cancellation released subscriptions.
        repeat(70) { check(client.observe(counter).first().count() == 21) }
        val queries = MutableStateFlow(query("SELECT note FROM drafts WHERE note=?", JsonArray(listOf(text("first")))))
        val firstQuery = CompletableDeferred<Unit>()
        val secondQuery = CompletableDeferred<Unit>()
        val dynamic = launch {
            client.observe(queries).collect {
                val notes = it.getValue("rows").jsonArray
                if (notes.isNotEmpty()) {
                    val value = notes.single().jsonArray.single().jsonObject.getValue("value").jsonPrimitive.content
                    if (value == "first") firstQuery.complete(Unit)
                    if (value == "note-21") secondQuery.complete(Unit)
                }
            }
        }
        firstQuery.await()
        queries.value = query("SELECT note FROM drafts WHERE note=?", JsonArray(listOf(text("note-21"))))
        secondQuery.await()
        dynamic.cancelAndJoin()
        client.close()
        client = RockyClient(ProcessTransport.open(host, database))
        inventory = InventoryClient(client)
        check(inventory.note(handle(1), "first", baseline = "0") == first)
        check(client.read(counter).count() == 21)
        check(inventory.stock() == 10L)
        // Strong reads bind a discovered coordinator head and report it explicitly.
        check(client.readStrong(query("SELECT remaining FROM stock", maxRows = 1)).getValue("state").jsonPrimitive.content == "ready")
        val reservation = request("invoke") {
            put("handle", handle(99)); put("method", "ReserveStock"); put("version", 1)
            put("arguments", bytes(byteArrayOf(3,0,0,0,0,0,0,0))); put("policy", handle(9))
        }
        check(client.call(reservation).getValue("state").jsonPrimitive.content == "queued")
        check(client.recoverCommand(handle(99)).getValue("state").jsonPrimitive.content == "confirmed_locally")
        check(inventory.stock() == 7L)
        client.close()
        println("Kotlin/JVM qualification passed: precise writes, handles, preconditions, scoped reads, Flow cancellation/coalescing/dynamic queries, process restart, strong reads and commands")
    }
}
