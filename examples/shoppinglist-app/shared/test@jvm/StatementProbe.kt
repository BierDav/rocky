package org.example.examples.shopping

import androidx.room.Room
import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteStatement
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.room.ROOM_PROFILE
import rocky.room.followRocky
import rocky.sqlite.RockySQLiteDriver
import java.io.File
import java.nio.file.Files
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicLong
import kotlin.test.Test

/** Temporary: times every statement Room sends for one tick. */
class StatementProbe {
    private val log = ConcurrentLinkedQueue<Triple<Long, String, Long>>()
    private val origin = System.nanoTime()

    private inner class TimedStatement(private val inner: SQLiteStatement, private val sql: String) : SQLiteStatement by inner {
        override fun step(): Boolean {
            val started = System.nanoTime()
            return try {
                inner.step()
            } finally {
                log += Triple(started - origin, "${Thread.currentThread().name.take(28)} | $sql", System.nanoTime() - started)
            }
        }
    }
    private inner class TimedConnection(private val inner: SQLiteConnection) : SQLiteConnection by inner {
        override fun prepare(sql: String): SQLiteStatement = TimedStatement(inner.prepare(sql), sql)
    }
    private inner class TimedDriver(private val inner: SQLiteDriver) : SQLiteDriver by inner {
        override fun open(fileName: String): SQLiteConnection = TimedConnection(inner.open(fileName))
    }

    @Test fun `statements of one tick`() = runBlocking {
        if (System.getenv("ROCKY_LATENCY_PROBE") != "1") return@runBlocking
        val root = Files.createTempDirectory("rocky-statements").toFile()
        val rocky = RockyDatabase.create(File(root, "device"), ShoppingSchema, PeerTransport.Tcp("127.0.0.1:0"))
        val room = Room.databaseBuilder<ShoppingDatabase>(File(root, "device/shopping").absolutePath)
            .setDriver(TimedDriver(RockySQLiteDriver(rocky, ROOM_PROFILE))).setQueryCoroutineContext(Dispatchers.IO).build()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            scope.launch { room.followRocky(rocky) }
            val repository = ShoppingRepository(room, rocky)
            val list = repository.createList("Groceries")
            repeat(50) { repository.add(list, "Item $it") }
            val shown = MutableStateFlow<List<ShoppingItem>>(emptyList())
            scope.launch { repository.observe(list).collect { shown.value = it } }
            val first = shown.first { it.size == 50 }.first()
            var completed = first.completed
            repeat(60) { tap ->
                completed = !completed
                if (tap == 59) {
                    delay(300)
                    log.clear()
                }
                val started = System.nanoTime()
                repository.check(list, first.id, completed)
                val wrote = System.nanoTime()
                shown.first { rows -> rows.first { it.id == first.id }.completed == completed }
                if (tap == 59) {
                    delay(300)
                    println("STATEMENTS write=%.2f ms visible=%.2f ms".format((wrote - started) / 1e6, (System.nanoTime() - started) / 1e6 - 300))
                    log.sortedBy { it.first }.forEach { (at, what, took) ->
                        println("STATEMENTS +%7.2f ms took %6.2f ms  %s".format((at - (started - origin)) / 1e6, took / 1e6, what.take(150)))
                    }
                }
            }
        } finally {
            scope.cancel()
            room.close()
            rocky.close()
            root.deleteRecursively()
        }
    }
}
