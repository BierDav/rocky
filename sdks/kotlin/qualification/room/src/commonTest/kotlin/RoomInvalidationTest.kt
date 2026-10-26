package rocky.room.qualification

import androidx.room.Room
import androidx.room.immediateTransaction
import androidx.room.useWriterConnection
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.test.runTest
import rocky.*
import rocky.room.RoomAvailability
import rocky.room.buildOnRocky
import rocky.room.createRockyPartition
import rocky.room.followRocky
import rocky.room.managedRockyRoom
import rocky.room.setRockyDatabase
import rocky.room.settledOnRocky
import rocky.testing.testRepositoryRoot
import kotlin.test.*
import rocky.testing.TestFile as File
import rocky.testing.TestFiles as Files

/** DAO observers follow every commit of Rocky's tables, sync writes included, with no trigger on Room's side. */
class RoomInvalidationTest {
    private val root = File(testRepositoryRoot())
    private val schema = schemaFromRoom(
        "shop",
        File(root, "sdks/kotlin/qualification/room/schemas/rocky.room.qualification.ShopDatabase/1.json").readText(),
    )
    private fun loopback(): PeerTransport = PeerTransport.Iroh.isolated()
    private fun item(key: String, title: String) = ShopItem(key, "l1", title)

    @Test fun sealRebootstrapKeepsTheObserverUntilVerifiedRowsReturn() = runTest {
        var reads = 0
        val observed = flow {
            reads++
            if (reads == 1) emit(listOf("before seal"))
            if (reads < 3) throw RockyException("blocked", true, "Waiting for verified rows")
            emit(listOf("verified seal"))
        }.settledOnRocky().toList()
        assertEquals(listOf(listOf("before seal"), listOf("verified seal")), observed)
        assertEquals(3, reads)
    }

    @Test fun readTimeoutRetriesCommittedRowsAndPermanentErrorsStillFail() = runTest {
        var reads = 0
        val observed = flow {
            reads++
            if (reads == 1) throw RockyException("transaction_timeout", true, "Read lease expired")
            emit(listOf("committed"))
        }.settledOnRocky().first()
        assertEquals(listOf("committed"), observed)
        assertEquals(2, reads)

        val failure = RockyException("denied", false, "session authority changed; transaction was rolled back: Timeout")
        var failedReads = 0
        val failed = flow<List<String>> {
            failedReads++
            throw failure
        }.settledOnRocky()
        assertSame(failure, assertFailsWith<RockyException> { failed.first() })
        assertEquals(1, failedReads)
    }

    @Test fun managedInstallationOwnsAcquisitionCloseAndBackgroundHealth() = runBlocking<Unit> {
        val directory = Files.createTempDirectory("managed-room").toFile()
        var opened: RockyDatabase? = null
        try {
            val installation = managedRockyRoom(
                open = { Rocky.configure(File(directory, "owned").path, schema) { networking { enabled = false } }.create().also { opened = it } },
                builder = { Room.databaseBuilder<ShopDatabase>(File(directory, "owned.room").path) },
            )
            var reads = 0
            val observed = installation.observeAvailability {
                flow {
                    emit(listOf("committed"))
                    if (reads++ == 0) throw RockyException("blocked", true, "Waiting for a verified base")
                }
            }.toList()
            assertTrue(observed[0] is RoomAvailability.Ready)
            assertEquals(listOf("committed"), (observed[1] as RoomAvailability.Waiting).previous)
            assertTrue(observed[2] is RoomAvailability.Ready)
            val problem = IllegalStateException("lifecycle failure")
            installation.bindLifecycle(flow { throw problem })
            assertSame(problem, withTimeout(5_000) { installation.failure.await() })
            assertTrue(Row.decode(installation.rocky.read(query("SELECT * FROM lists"))).isEmpty())
            installation.close()
            installation.close()
            assertFails { opened!!.read(query("SELECT * FROM lists")) }

            val borrowed = RockyDatabase.local(File(directory, "borrowed").path, schema)
            val attached = Room.databaseBuilder<ShopDatabase>(File(directory, "borrowed.room").path).buildOnRocky(borrowed)
            attached.room.useWriterConnection { writer ->
                writer.immediateTransaction {
                    val nested = assertFailsWith<IllegalStateException> {
                        attached.room.createRockyPartition(borrowed, "lists") { error("The callback must not run") }
                    }
                    assertEquals("Partition creation must start outside a Room transaction", nested.message)
                }
            }
            attached.close()
            assertTrue(Row.decode(borrowed.read(query("SELECT * FROM lists"))).isEmpty())
            borrowed.close()

            val problemOnBuild = IllegalArgumentException("builder failed")
            assertEquals(
                problemOnBuild.message,
                assertFailsWith<IllegalArgumentException> {
                    managedRockyRoom<ShopDatabase>(
                        open = { RockyDatabase.local(File(directory, "failed").path, schema).also { opened = it } },
                        builder = { throw problemOnBuild },
                    )
                }.message,
            )
            assertFails { opened!!.read(query("SELECT * FROM lists")) }
        } finally {
            opened?.close()
            directory.deleteRecursively()
        }
    }

    @Test fun cancellationAfterAcquisitionClosesTheOwnedInstallation() = runBlocking<Unit> {
        val directory = Files.createTempDirectory("managed-cancellation").toFile()
        val ready = CompletableDeferred<RockyDatabase>()
        val release = CompletableDeferred<Unit>()
        try {
            val opening = launch {
                managedRockyRoom(
                    open = {
                        withContext(NonCancellable) {
                            RockyDatabase.local(File(directory, "database").path, schema).also {
                                ready.complete(it)
                                release.await()
                            }
                        }
                    },
                    builder = { Room.databaseBuilder<ShopDatabase>(File(directory, "room").path) },
                )
            }
            val database = ready.await()
            opening.cancel()
            release.complete(Unit)
            opening.join()
            assertFails { database.read(query("SELECT * FROM lists")) }
        } finally {
            if (ready.isCompleted) ready.await().close()
            directory.deleteRecursively()
        }
    }

    @Test fun daoObserversFireOnLocalAndSyncWrites() = runBlocking {
        val directory = Files.createTempDirectory("room-invalidation").toFile()
        val a = RockyDatabase.create(File(directory, "a").path, schema, loopback(), automaticSync = false)
        var b: RockyDatabase? = null
        val work = mutableListOf<Job>()
        try {
            b = RockyDatabase.join(
                File(directory, "b").path,
                schema,
                a.pairing(GrantProfile.Writer(setOf("lists", "items"))).uri,
                loopback(),
                automaticSync = false,
            )
            val roomA = Room.databaseBuilder<ShopDatabase>(File(directory, "a.room").path).setRockyDatabase(a).build()
            val roomB = Room.databaseBuilder<ShopDatabase>(File(directory, "b.room").path).setRockyDatabase(b).build()
            work += launch(Dispatchers.IO) { roomB.followRocky(b) }
            val seen = Channel<List<String>>(Channel.UNLIMITED)
            work += launch(Dispatchers.IO) { roomB.shop().observe("l1").collect { items -> seen.send(items.map { it.title }) } }
            suspend fun until(expected: List<String>) = withTimeout(20_000) {
                while (true) if (seen.receive() == expected) break
            }
            until(emptyList())
            // A local DAO write: Room's own refresh after its transaction.
            roomB.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk")))
            until(listOf("Milk"))
            // Another device's write arrives by sync, never through Room's connection.
            val eggs = roomA.shop().createItems(listOf(item("i2", "Eggs"))).single()
            assertTrue(b.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            until(listOf("Eggs", "Milk"))
            roomA.shop().update(eggs.copy(title = "Free-range eggs"))
            b.syncOnce()
            until(listOf("Free-range eggs", "Milk"))
            assertEquals(listOf("Free-range eggs", "Milk"), roomB.shop().items("l1").map { it.title })
            work.forEach { it.cancelAndJoin() }
            work.clear()
            roomA.close()
            roomB.close()
        } finally {
            work.forEach { it.cancelAndJoin() }
            b?.close()
            a.close()
            directory.deleteRecursively()
        }
    }
}
