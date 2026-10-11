package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import rocky.PeerTransport
import java.io.File
import java.nio.file.Files
import kotlin.test.Test

/**
 * Measures what a tap costs on one device: the DAO write, and the time until the observed list shows it. A
 * measurement, not a gate: it runs only with `ROCKY_LATENCY_PROBE=1` and prints its percentiles.
 */
class LocalLatencyProbe {
    private fun percentiles(name: String, nanos: List<Long>) {
        val sorted = nanos.sorted()
        fun at(p: Double) = sorted[minOf(sorted.size - 1, (sorted.size * p).toInt())] / 1e6
        println("LATENCY %-22s n=%d p50=%.2f ms p95=%.2f ms max=%.2f ms".format(name, sorted.size, at(0.50), at(0.95), sorted.last() / 1e6))
    }

    @Test fun `ticking an item reaches its observer`() = runBlocking {
        if (System.getenv("ROCKY_LATENCY_PROBE") != "1") return@runBlocking
        val items = System.getenv("ROCKY_LATENCY_ITEMS")?.toInt() ?: 50
        val taps = System.getenv("ROCKY_LATENCY_TAPS")?.toInt() ?: 200
        val root = Files.createTempDirectory("rocky-latency").toFile()
        val node = ShoppingNode.create(
            ShoppingPlatform(
                File(root, "device").absolutePath,
                "device",
                room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
                transport = { PeerTransport.Tcp("127.0.0.1:0") },
            ),
        )
        val observing = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val repository = node.repository
            val list = repository.createList("Groceries")
            val adds = (1..items).map { index ->
                val started = System.nanoTime()
                repository.add(list, "Item $index")
                System.nanoTime() - started
            }
            // The screen's own observation: the latest rows, as its state flow holds them.
            val shown = MutableStateFlow<List<ShoppingItem>>(emptyList())
            observing.launch { repository.observe(list).collect { shown.value = it } }
            val first = shown.first { it.size == items }.first()

            val writes = ArrayList<Long>()
            val visible = ArrayList<Long>()
            var completed = first.completed
            repeat(taps + WARMUP) { tap ->
                completed = !completed
                val started = System.nanoTime()
                val write = async(Dispatchers.Default) {
                    repository.check(list, first.id, completed)
                    System.nanoTime() - started
                }
                shown.first { rows -> rows.first { it.id == first.id }.completed == completed }
                val seen = System.nanoTime() - started
                if (tap >= WARMUP) {
                    writes += write.await()
                    visible += seen
                } else {
                    write.await()
                }
            }
            println("LATENCY items=$items taps=$taps")
            percentiles("add (write returns)", adds.drop(minOf(WARMUP, adds.size - 1)))
            percentiles("tick (write returns)", writes)
            percentiles("tick (list shows it)", visible)
        } finally {
            observing.cancel()
            node.close()
            root.deleteRecursively()
        }
    }

    private companion object {
        const val WARMUP = 20
    }
}
