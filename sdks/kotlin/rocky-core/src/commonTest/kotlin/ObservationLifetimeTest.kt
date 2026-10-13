package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*
import kotlin.test.*

class ObservationLifetimeTest {
    @Test
    fun closeWaitsForRegistrationAndUnsubscribeBeforeClosingTransport() = runBlocking {
        withTimeout(2_000) {
            val registering = CompletableDeferred<Unit>()
            val registered = CompletableDeferred<Unit>()
            val cleaning = CompletableDeferred<Unit>()
            val cleaned = CompletableDeferred<Unit>()
            var transportClosed = false
            var nextCalls = 0
            val client = RockyClient(object : Transport {
                override suspend fun call(request: JsonObject): JsonObject {
                    check(!transportClosed) { "database closed" }
                    return when (request.getValue("type").jsonPrimitive.content) {
                        "subscribe" -> {
                            registering.complete(Unit)
                            registered.await()
                            buildJsonObject { put("subscription", "1") }
                        }

                        "unsubscribe" -> {
                            assertEquals("1", request.getValue("subscription").jsonPrimitive.content)
                            cleaning.complete(Unit)
                            cleaned.await()
                            buildJsonObject {}
                        }

                        "next" -> {
                            nextCalls++
                            awaitCancellation()
                        }

                        else -> error("unexpected call")
                    }
                }
                override suspend fun close() {
                    transportClosed = true
                }
            })
            val collector = launch { client.observe(query("SELECT id FROM items")).collect() }
            registering.await()
            val closing = launch(start = CoroutineStart.UNDISPATCHED) { client.close() }
            assertFalse(closing.isCompleted)
            assertFalse(transportClosed)
            registered.complete(Unit)
            cleaning.await()
            assertFalse(transportClosed)
            cleaned.complete(Unit)
            closing.join()
            collector.join()
            assertTrue(collector.isCancelled)
            assertTrue(transportClosed)
            assertEquals(0, nextCalls)
            // A late collector never registers on the closed transport.
            val late = launch { client.observe(query("SELECT id FROM items")).collect() }
            late.join()
            assertTrue(late.isCancelled)
            client.close()
        }
    }

    @Test
    fun closeCancelsStatusAndActivityWaitsWithoutCancellingTheirOwner() = runBlocking {
        withTimeout(2_000) {
            val readinessWaiting = CompletableDeferred<Unit>()
            val activityWaiting = CompletableDeferred<Unit>()
            var waits = 0
            val client = RockyClient(object : Transport {
                override suspend fun call(request: JsonObject): JsonObject = when (request.getValue("type").jsonPrimitive.content) {
                    "status" -> buildJsonObject { put("token", "1") }

                    "activity" -> buildJsonObject {
                        put("token", "1")
                        put("events", JsonArray(emptyList()))
                    }

                    "status_next", "activity_next" -> {
                        waits++
                        if (request.getValue("type").jsonPrimitive.content == "status_next") readinessWaiting.complete(Unit) else activityWaiting.complete(Unit)
                        try {
                            awaitCancellation()
                        } finally {
                            waits--
                        }
                    }

                    else -> error("unexpected call")
                }
                override suspend fun close() {
                    assertEquals(0, waits)
                }
            })
            val readiness = launch { client.statusChanges().collect() }
            val activity = launch { client.activity().collect() }
            readinessWaiting.await()
            activityWaiting.await()
            client.close()
            joinAll(readiness, activity)
            assertTrue(readiness.isCancelled)
            assertTrue(activity.isCancelled)
            currentCoroutineContext().ensureActive()
        }
    }

    @Test
    fun observationFailuresStillReachTheCallerAndReleaseTheSubscription() = runBlocking {
        var unsubscribed = false
        val failure = RockyException("denied", false, "read denied")
        val client = RockyClient(object : Transport {
            override suspend fun call(request: JsonObject): JsonObject = when (request.getValue("type").jsonPrimitive.content) {
                "subscribe" -> buildJsonObject { put("subscription", "1") }

                "next" -> throw failure

                "unsubscribe" -> {
                    unsubscribed = true
                    buildJsonObject {}
                }

                else -> error("unexpected call")
            }
            override suspend fun close() {}
        })
        assertSame(failure, assertFailsWith<RockyException> { client.observe(query("SELECT id FROM items")).collect() })
        assertTrue(unsubscribed)
        client.close()
    }
}
