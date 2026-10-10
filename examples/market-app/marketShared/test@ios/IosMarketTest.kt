@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.market

import kotlinx.coroutines.runBlocking
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import rocky.*
import kotlin.test.*
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource

/** Native application handler and durable tickets; the three-voter procedure is a separate e2e gate. */
class IosMarketTest {
    @Test fun theLastItemIsReservedOnceAndItsTicketSurvivesReopen() = runBlocking<Unit> {
        val began = TimeSource.Monotonic.markNow()
        val root = NSTemporaryDirectory() + "market-ios-" + NSUUID().UUIDString
        val coordinator = DevelopmentCoordinator()
        val commands = CommandRuntime(coordinator, MarketHandlers)
        var database = RockyDatabase.local(root, MarketSchema, commands = commands)
        try {
            coordinator.register(database.commandIdentity())
            val stock = RockyDatabase.newHandle()
            database.submit(Restock, RestockArguments("bolt", 1), stock)
            assertEquals(Outcome.Accepted(Remaining(1)), database.run(Restock, stock).outcome)
            val first = RockyDatabase.newHandle()
            val second = RockyDatabase.newHandle()
            database.submit(Reserve, ReserveArguments("bolt", 1, first), first)
            assertEquals(Outcome.Accepted(Remaining(0)), database.run(Reserve, first).outcome)
            database.submit(Reserve, ReserveArguments("bolt", 1, second), second)
            assertEquals(Outcome.Rejected(Shortfall(0, 1)), database.run(Reserve, second).outcome)
            database.close()
            database = RockyDatabase.open(root, MarketSchema, commands = commands)
            assertEquals(Outcome.Accepted(Remaining(0)), database.ticket(Reserve, first)?.outcome)
            assertEquals(Outcome.Rejected(Shortfall(0, 1)), database.ticket(Reserve, second)?.outcome)
        } finally {
            database.close()
            coordinator.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
        val elapsed = began.elapsedNow()
        println("iOS integration test: $elapsed")
        assertTrue(elapsed < 2.seconds, "Integration budget exceeded: $elapsed")
    }
}
