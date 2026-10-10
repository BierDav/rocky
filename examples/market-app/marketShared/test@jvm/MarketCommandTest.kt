package org.example.examples.market

import kotlinx.coroutines.*
import org.junit.jupiter.api.BeforeAll
import rocky.*
import java.io.File
import java.nio.file.Files
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.*

/** Typed strong commands decided by a handler written in Kotlin, through the packaged runtime. */
class MarketCommandTest {
    companion object {
        @JvmStatic @BeforeAll
        fun load() = loadNative()
    }

    private fun directory(): File = Files.createTempDirectory("market-commands-test").toFile()
    private fun tcp(): PeerTransport = PeerTransport.Tcp("127.0.0.1:0")
    private fun handle() = RockyDatabase.newHandle()

    /** Business columns only: without the trailing `id` and `row_kind`. */
    private fun List<Any?>.fields() = dropLast(2)

    @JvmName("rowFields")
    private fun List<List<Any?>>.fields() = map { it.fields() }
    private suspend fun RockyDatabase.restock(quantity: Long) {
        val handle = handle()
        submit(Restock, RestockArguments("bolt", quantity), handle)
        assertEquals(TicketState.Applied, run(Restock, handle).state)
    }

    @Test fun `a Kotlin handler confirms one of two competing reservations and rejects the other`() = runBlocking {
        val root = directory()
        val coordinator = DevelopmentCoordinator()
        val executor = RockyDatabase.create(
            File(root, "a"),
            MarketSchema,
            tcp(),
            automaticSync = false,
            commands = CommandRuntime(coordinator, MarketHandlers),
        )
        // A buyer holds the definitions and no handler: it invokes and reads, and cannot execute.
        val caller = RockyDatabase.join(
            File(root, "b"),
            MarketSchema,
            executor.pairing(Market.Buyer).uri,
            PeerTransport.Tcp(),
            automaticSync = false,
            commands = CommandRuntime(coordinator),
        )
        try {
            val owner = executor.commandIdentity()
            assertTrue(owner.executes && !caller.commandIdentity().executes)
            coordinator.register(owner, callers = listOf(caller.commandIdentity()))
            executor.restock(1)

            val (mine, theirs) = handle() to handle()
            val queued = caller.submit(Reserve, ReserveArguments("bolt", 1, "theirs"), theirs)
            assertEquals(TicketState.Queued, queued.state)
            assertNull(queued.outcome)
            // The same handle and arguments are the same ticket; other arguments conflict.
            assertEquals(queued, caller.submit(Reserve, ReserveArguments("bolt", 1, "theirs"), theirs))
            assertEquals("conflict", assertFailsWith<RockyException> { caller.submit(Reserve, ReserveArguments("bolt", 2, "theirs"), theirs) }.code)
            executor.submit(Reserve, ReserveArguments("bolt", 1, "mine"), mine)
            assertEquals(1, coordinator.sequence(owner))

            val accepted = executor.run(Reserve, mine)
            assertEquals(TicketState.Applied, accepted.state)
            assertEquals(Outcome.Accepted(Remaining(0)), accepted.outcome)
            // Delivered to the executor over the network; the rejection is a confirmed typed result.
            val rejected = caller.run(Reserve, theirs)
            assertNull(rejected.error)
            assertEquals(Outcome.Rejected(Shortfall(0, 1)), rejected.outcome)
            assertEquals(3, rejected.sequence)
            assertEquals(3, coordinator.sequence(owner))
            caller.syncOnce()
            assertEquals(TicketState.Applied, caller.ticket(Reserve, theirs)?.state)
            for (database in listOf(executor, caller)) {
                val stock = database.confirmedRows("stock")
                assertTrue(stock.complete)
                assertEquals(listOf(listOf<Any?>("bolt", 0L)), stock.rows.fields())
                assertEquals(listOf(listOf<Any?>("mine", "bolt", 1L)), database.confirmedRows("holds").rows.fields())
            }
            assertEquals(0, executor.runCommands() + caller.runCommands())
            // A protocol failure is an exception with a code, never a rejection.
            assertNull(caller.ticket(Reserve, handle()))
        } finally {
            caller.close()
            executor.close()
            root.deleteRecursively()
        }
    }

    @Test fun `a device an issuer granted Execute registers and executes without being an issuer`() = runBlocking<Unit> {
        val root = directory()
        val coordinator = DevelopmentCoordinator()
        // The issuer holds no handler; it grants Execute as a raw right to another device.
        val issuer = RockyDatabase.create(File(root, "a"), MarketSchema, tcp(), automaticSync = false, commands = CommandRuntime(coordinator))
        var executor: RockyDatabase? = null
        try {
            val all = setOf(Right.Read, Right.Insert, Right.Update, Right.Delete, Right.Invoke, Right.Execute, Right.Custody)
            val grants = listOf(Grant("stock", all), Grant("holds", all))
            val keys = Credentials.prepare(File(root, "x"))
            executor = RockyDatabase.install(
                File(root, "x"),
                MarketSchema,
                issuer.credentials.grant(keys, grants),
                transport = tcp(),
                commands = CommandRuntime(coordinator, MarketHandlers),
            )
            assertFalse(executor.isIssuer)
            assertEquals(GrantProfile.Grants(grants), executor.session().profile)
            val identity = executor.commandIdentity()
            assertTrue(identity.executes)
            coordinator.register(identity, callers = listOf(issuer.commandIdentity()))
            executor.restock(2)
            val mine = handle()
            executor.submit(Reserve, ReserveArguments("bolt", 1, "mine"), mine)
            val accepted = executor.run(Reserve, mine)
            assertEquals(TicketState.Applied, accepted.state)
            assertEquals(Outcome.Accepted(Remaining(1)), accepted.outcome)
            assertEquals(2, coordinator.sequence(identity))

            // A hold key names one item: reserving under it again adds to the hold, another item is refused.
            suspend fun reserve(item: String) = handle().let { handle ->
                executor.submit(Reserve, ReserveArguments(item, 1, "mine"), handle)
                executor.run(Reserve, handle).outcome
            }
            assertEquals(Outcome.Accepted(Remaining(0)), reserve("bolt"))
            assertEquals(listOf(listOf<Any?>("mine", "bolt", 2L)), executor.confirmedRows("holds").rows.fields())
            suspend fun restock(item: String, quantity: Long) = handle().let { handle ->
                executor.submit(Restock, RestockArguments(item, quantity), handle)
                executor.run(Restock, handle).outcome
            }
            assertEquals(Outcome.Accepted(Remaining(1)), restock("nut", 1))
            assertEquals(Outcome.Rejected(Shortfall(1, 1)), reserve("nut"))
            assertEquals(Outcome.Rejected(Unit), restock("bolt", 0))
            // Each new row took the next id; an updated row kept its own.
            suspend fun ids(table: String) = executor.confirmedRows(table).rows.associate { it[0] to it[it.size - 2] }
            assertEquals(mapOf<Any?, Any?>("bolt" to (1L shl 32) + 1, "nut" to (1L shl 32) + 2), ids("stock"))
            assertEquals(mapOf<Any?, Any?>("mine" to (1L shl 32) + 1), ids("holds"))
        } finally {
            executor?.close()
            issuer.close()
            root.deleteRecursively()
        }
    }

    @Test fun `a failing or slow handler fails one call and the same ticket recovers`() = runBlocking {
        val root = directory()
        val coordinator = DevelopmentCoordinator()
        val calls = AtomicInteger()
        val running = AtomicInteger()
        val overlapped = AtomicInteger()
        val flaky = Reserve handledBy { context, arguments ->
            if (running.incrementAndGet() > 1) overlapped.incrementAndGet()
            try {
                when (calls.incrementAndGet()) {
                    1 -> error("the handler threw")
                    2 -> Thread.sleep(400)
                }
                reserve.handler.handle(context, arguments)
            } finally {
                running.decrementAndGet()
            }
        }
        val runtime = CommandRuntime(coordinator, listOf(flaky, restock), handlerDeadlineMillis = 250)
        var executor = RockyDatabase.create(File(root, "a"), MarketSchema, tcp(), automaticSync = false, commands = runtime)
        try {
            val owner = executor.commandIdentity()
            coordinator.register(owner)
            executor.restock(3)
            val first = handle()
            val submitted = executor.submit(Reserve, ReserveArguments("bolt", 1, "first"), first)

            // An exception, then a missed deadline: each fails its call and leaves the ticket queued.
            repeat(2) {
                val failed = executor.run(Reserve, first)
                assertEquals("handler_unavailable", failed.error?.code)
                assertEquals(true, failed.error?.retryable)
                assertEquals(TicketState.Queued, failed.state)
            }
            assertEquals(1, coordinator.sequence(owner))
            assertEquals(listOf(listOf<Any?>("bolt", 3L)), executor.confirmedRows("stock").rows.fields())
            var ticket = executor.run(Reserve, first)
            // The slow call still holds the handler's thread for a moment.
            while (ticket.state != TicketState.Applied) {
                assertEquals(true, ticket.error?.retryable)
                delay(50)
                ticket = executor.run(Reserve, first)
            }
            assertEquals(Outcome.Accepted(Remaining(2)), ticket.outcome)
            assertEquals(2, coordinator.sequence(owner))

            // The coordinator accepts and loses its answer; the process then restarts.
            val second = handle()
            executor.submit(Reserve, ReserveArguments("bolt", 1, "second"), second)
            coordinator.dropNextReply()
            val unknown = executor.run(Reserve, second)
            assertEquals(TicketState.OutcomeUnknown, unknown.state)
            assertEquals("unknown_confirmation", unknown.error?.code)
            assertEquals(TicketState.OutcomeUnknown, executor.cancel(Reserve, second).state)
            val decided = calls.get()
            executor.close()
            executor = RockyDatabase.resume(File(root, "a"), MarketSchema, tcp(), automaticSync = false, commands = runtime)
            assertEquals(TicketState.OutcomeUnknown, executor.ticket(Reserve, second)?.state)
            val recovered = executor.run(Reserve, second)
            assertEquals(TicketState.Applied, recovered.state)
            assertEquals(Outcome.Accepted(Remaining(1)), recovered.outcome)
            // The retained preparation was confirmed: no second decision and no second transition.
            assertEquals(decided, calls.get())
            assertEquals(3, coordinator.sequence(owner))
            assertEquals(submitted.handle, executor.ticket(Reserve, first)?.handle)

            // Concurrent runs reach the handler one at a time; only one reservation is left to accept.
            val (third, fourth) = handle() to handle()
            executor.submit(Reserve, ReserveArguments("bolt", 1, "third"), third)
            executor.submit(Reserve, ReserveArguments("bolt", 1, "fourth"), fourth)
            val outcomes = listOf(async { executor.run(Reserve, third) }, async { executor.run(Reserve, fourth) }).awaitAll().map { it.outcome }
            assertEquals(setOf(Outcome.Accepted(Remaining(0)), Outcome.Rejected(Shortfall(0, 1))), outcomes.toSet())
            assertEquals(0, overlapped.get())
            assertEquals(5, coordinator.sequence(owner))

            // A cancelled ticket never runs.
            val withdrawn = handle()
            executor.submit(Reserve, ReserveArguments("bolt", 1, "withdrawn"), withdrawn)
            assertEquals(TicketState.Cancelled, executor.cancel(Reserve, withdrawn).state)
            assertEquals("rejected", executor.run(Reserve, withdrawn).error?.code)
            assertEquals(5, coordinator.sequence(owner))
        } finally {
            executor.close()
            root.deleteRecursively()
        }
    }

    @Test fun `handler reads select keys and bounded ranges and expire after return`() = runBlocking<Unit> {
        val root = directory()
        val coordinator = DevelopmentCoordinator()
        val retained = java.util.concurrent.atomic.AtomicReference<CommandContext>()
        val probe = Reserve handledBy { context, arguments ->
            retained.set(context)
            assertEquals(listOf<Any?>("bolt", 2L), context.get("stock", "bolt")?.fields())
            assertNull(context.get("stock", "missing"))
            assertEquals(listOf(listOf<Any?>("alpha", 3L)), context.scan("stock", limit = 1).fields())
            assertEquals(
                listOf(listOf<Any?>("bolt", 2L)),
                context.scan("stock", lower = listOf("bolt"), upper = listOf("cedar"), limit = 10).fields(),
            )
            assertEquals(emptyList(), context.scan("stock", lower = listOf("bolt"), upper = listOf("bolt"), limit = 1))
            assertFails { context.get("undeclared", "bolt") }
            assertFails { context.get("stock") }
            assertFails { context.get("stock", null) }
            assertFails { context.scan("stock", limit = 0) }
            assertFails { context.scan("stock", limit = 1025) }
            assertFails { context.scan("stock", lower = listOf("cedar"), upper = listOf("alpha"), limit = 1) }
            reserve.handler.handle(context, arguments)
        }
        val executor = RockyDatabase.create(
            File(root, "a"),
            MarketSchema,
            tcp(),
            automaticSync = false,
            commands = CommandRuntime(coordinator, listOf(probe, restock)),
        )
        try {
            coordinator.register(executor.commandIdentity())
            for ((item, quantity) in listOf("cedar" to 4L, "bolt" to 2L, "alpha" to 3L)) {
                val handle = handle()
                executor.submit(Restock, RestockArguments(item, quantity), handle)
                assertEquals(TicketState.Applied, executor.run(Restock, handle).state)
            }
            val handle = handle()
            executor.submit(Reserve, ReserveArguments("bolt", 1, "mine"), handle)
            val ticket = executor.run(Reserve, handle)
            assertNull(ticket.error)
            assertEquals(Outcome.Accepted(Remaining(1)), ticket.outcome)
            assertFails { retained.get().get("stock", "bolt") }
            assertFails { retained.get().scan("stock", limit = 1) }
        } finally {
            executor.close()
            root.deleteRecursively()
        }
    }
}
