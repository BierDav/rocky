package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.serialization.json.*
import kotlin.test.*

class PartitionObservationTest {
    @Test
    fun hundredsOfPartitionsShareWaitsAndFollowMetadataWithoutStatusChange() = runBlocking {
        withTimeout(5_000) {
            val waiting = CompletableDeferred<Unit>()
            val activityWaiting = CompletableDeferred<Unit>()
            val firstSnapshot = CompletableDeferred<Unit>()
            val changed = Channel<Unit>()
            var waits = 0
            var activeWaits = 0
            var maximumWaits = 0
            var revision = 0
            val client = RockyClient(object : Transport {
                override suspend fun call(request: JsonObject): JsonObject = when (request.getValue("type").jsonPrimitive.content) {
                    "status" -> buildJsonObject { put("token", 0) }

                    "status_next" -> {
                        waits++
                        activeWaits++
                        maximumWaits = maxOf(maximumWaits, activeWaits)
                        try {
                            waiting.complete(Unit)
                            awaitCancellation()
                        } finally {
                            activeWaits--
                        }
                    }

                    "activity" -> buildJsonObject {
                        put("token", 0)
                        put("events", JsonArray(emptyList()))
                    }

                    "activity_next" -> {
                        activityWaiting.complete(Unit)
                        changed.receive()
                        revision++
                        buildJsonObject {
                            put("token", revision)
                            put(
                                "events",
                                buildJsonArray {
                                    add(
                                        buildJsonObject {
                                            put("sequence", revision)
                                            put("at", 0)
                                            put("kind", "partition_shared")
                                        },
                                    )
                                },
                            )
                        }
                    }

                    "partition_status" -> buildJsonObject {
                        put("epochs", JsonArray(emptyList()))
                        put(
                            "tables",
                            buildJsonArray {
                                add(
                                    buildJsonObject {
                                        put("table", request.getValue("group"))
                                        put("partition", "scope")
                                        put("holding", if (revision == 0) "held" else "evicted")
                                        put("epochs", JsonArray(emptyList()))
                                    },
                                )
                            },
                        )
                    }

                    else -> error("Unexpected request: $request")
                }
                override suspend fun close() {}
            })
            try {
                val groups = (1..256).map { client.partitions.group("group$it") }
                val observed = async { client.partitions.observeStatuses(groups).onEach { firstSnapshot.complete(Unit) }.take(2).toList() }
                firstSnapshot.await()
                waiting.await()
                activityWaiting.await()
                assertEquals(1, waits)
                changed.send(Unit)
                val snapshots = observed.await()
                assertEquals((1..256).map { "group$it" }, snapshots[0].map { it.tables.single().table })
                assertTrue(snapshots[0].all { it.isHeld })
                assertTrue(snapshots[1].none { it.isHeld })
                assertEquals(1, maximumWaits)
                assertEquals(1, waits)
            } finally {
                client.close()
            }
        }
    }
}
