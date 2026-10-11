package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.PeerTransport
import java.io.File
import java.nio.file.Files
import kotlin.test.Test

/**
 * Measures what a tap costs on one device: the DAO write, and the time until the observed list shows it. A
 * measurement, not a gate: it runs only with `ROCKY_LATENCY_PROBE=1` and prints its percentiles. See
 * `docs/runtime/local-latency.md`.
 */
class LocalLatencyProbe {
    private val items = System.getenv("ROCKY_LATENCY_ITEMS")?.toInt() ?: 50
    private val taps = System.getenv("ROCKY_LATENCY_TAPS")?.toInt() ?: 200

    /** A person pauses between taps; 0 measures a burst, where background work meets the next tap. */
    private val pause = System.getenv("ROCKY_LATENCY_PAUSE_MS")?.toLong() ?: 20

    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name).absolutePath,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Tcp("127.0.0.1:0") },
    )
    private fun percentiles(name: String, nanos: List<Long>) {
        val sorted = nanos.sorted()
        fun at(p: Double) = sorted[minOf(sorted.size - 1, (sorted.size * p).toInt())] / 1e6
        println("LATENCY %-24s n=%d p50=%.2f ms p95=%.2f ms max=%.2f ms".format(name, sorted.size, at(0.50), at(0.95), sorted.last() / 1e6))
    }

    /** The rows a screen observing [list] on [node] holds, as its state flow would. */
    private fun CoroutineScope.shown(node: ShoppingNode, list: Long): MutableStateFlow<List<ShoppingItem>> {
        val shown = MutableStateFlow<List<ShoppingItem>>(emptyList())
        launch { node.repository.observe(list).collect { shown.value = it } }
        return shown
    }

    /** Ticks one item of [list] on [node] and times the write, its own observer and, when given, a [peer]'s. */
    private suspend fun measure(scenario: String, node: ShoppingNode, list: Long, peer: ShoppingNode? = null) = coroutineScope {
        val observing = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val repository = node.repository
            val adds = (1..items).map { index ->
                val started = System.nanoTime()
                repository.add(list, "Item $index")
                System.nanoTime() - started
            }
            val shown = observing.shown(node, list)
            val remote = peer?.let { observing.shown(it, list) }
            val first = shown.first { it.size == items }.first()
            remote?.first { it.size == items }

            val writes = ArrayList<Long>()
            val visible = ArrayList<Long>()
            val synced = ArrayList<Long>()
            var completed = first.completed
            repeat(taps + WARMUP) { tap ->
                completed = !completed
                delay(pause)
                val started = System.nanoTime()
                val write = async(Dispatchers.Default) {
                    repository.check(list, first.id, completed)
                    System.nanoTime() - started
                }
                shown.first { rows -> rows.first { it.id == first.id }.completed == completed }
                val seen = System.nanoTime() - started
                val arrived = remote?.let { rows ->
                    rows.first { it.first { item -> item.id == first.id }.completed == completed }
                    System.nanoTime() - started
                }
                val wrote = write.await()
                if (tap >= WARMUP) {
                    writes += wrote
                    visible += seen
                    arrived?.let { synced += it }
                }
            }
            println("LATENCY $scenario: items=$items taps=$taps pause=${pause}ms")
            percentiles("add (write returns)", adds.drop(minOf(WARMUP, adds.size - 1)))
            percentiles("tick (write returns)", writes)
            percentiles("tick (list shows it)", visible)
            if (synced.isNotEmpty()) percentiles("tick (peer shows it)", synced)
        } finally {
            observing.cancel()
        }
    }

    @Test fun `ticking an item reaches its observer`() = runBlocking {
        if (System.getenv("ROCKY_LATENCY_PROBE") != "1") return@runBlocking
        val root = Files.createTempDirectory("rocky-latency").toFile()
        val node = ShoppingNode.create(platform(root, "device"))
        try {
            measure("alone", node, node.repository.createList("Groceries"))
        } finally {
            node.close()
            root.deleteRecursively()
        }
    }

    @Test fun `ticking an item while a peer syncs`() = runBlocking {
        if (System.getenv("ROCKY_LATENCY_PROBE") != "1") return@runBlocking
        val root = Files.createTempDirectory("rocky-latency-peer").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val trusted = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val laptop = ShoppingNode.join(platform(root, "laptop"), trusted.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
            withTimeout(30_000) { trusted.household.devices().first { devices -> devices.any { it.node == laptop.self } } }
            val list = trusted.repository.createList("Groceries")
            withTimeout(60_000) { laptop.repository.lists().first { lists -> lists.any { it.id == list } } }
            measure("with a syncing peer", trusted, list, laptop)
        } finally {
            withContext(NonCancellable) { opened.reversed().forEach { it.close() } }
            root.deleteRecursively()
        }
    }

    private companion object {
        const val WARMUP = 20
    }
}
