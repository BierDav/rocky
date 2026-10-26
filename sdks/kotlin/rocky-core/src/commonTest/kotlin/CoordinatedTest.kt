package rocky

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import kotlin.test.*

class CoordinatedTest {
    /** Answers each call from `answer` and records it. */
    private class Recording(val answer: (JsonObject) -> JsonObject) : Transport {
        val calls = mutableListOf<JsonObject>()
        override suspend fun call(request: JsonObject): JsonObject {
            calls.add(request)
            return answer(request)
        }
        override suspend fun close() {}
    }
    private fun JsonObject.sql(): String? = this["sql"]?.jsonPrimitive?.content

    @Test
    fun aCoordinatedWriteBeginsAndCommitsAsOneCoordinatedTransaction() = runBlocking {
        val transport = Recording { request ->
            when (request.sql()) {
                "SELECT COUNT(*) FROM items" -> buildJsonObject {
                    put(
                        "rows",
                        buildJsonArray {
                            add(
                                buildJsonArray {
                                    add(
                                        buildJsonObject {
                                            put("type", "integer")
                                            put("value", "4")
                                        },
                                    )
                                },
                            )
                        },
                    )
                    put("changes", "0")
                }

                "COMMIT" -> buildJsonObject {
                    put(
                        "committed",
                        buildJsonObject {
                            put("position", "5")
                            put("hash", "ab")
                            put("frames", 1)
                        },
                    )
                }

                else -> buildJsonObject {
                    put("changes", "1")
                    put("rows", JsonArray(emptyList()))
                }
            }
        }
        val committed = RockyClient(transport).coordinated(bytes(ByteArray(32))) {
            val count = query("SELECT COUNT(*) FROM items")[0].long(0)
            execute("INSERT INTO items(key,title) VALUES(?,?)", (count + 1).toString().padStart(4, '0'), "invoice")
            count + 1
        }
        assertEquals(CoordinatedCommit(5, "ab", 5L), committed)
        val begin = transport.calls.first()
        assertEquals("BEGIN", begin.sql())
        assertEquals(JsonPrimitive(true), begin["coordinated"])
        val commit = transport.calls.last()
        assertEquals("COMMIT", commit.sql())
        assertEquals(JsonPrimitive(true), commit["coordinated"])
    }

    @Test
    fun aRefusedCoordinatedCommitIsTypedAndNeverACommit() = runBlocking {
        val transport = Recording { request ->
            if (request.sql() == "COMMIT") throw RockyException("conflict", true, "coordinated write: Conflict")
            buildJsonObject {
                put("changes", "1")
                put("rows", JsonArray(emptyList()))
            }
        }
        val failure = assertFailsWith<RockyException> {
            RockyClient(transport).coordinated(bytes(ByteArray(32))) { execute("INSERT INTO items(key) VALUES('x')") }
        }
        assertEquals("conflict", failure.code)
        assertTrue(failure.retryable)
    }

    @Test
    fun aFailingBlockRollsTheCoordinatedTransactionBack() = runBlocking {
        val transport = Recording {
            buildJsonObject {
                put("changes", "0")
                put("rows", JsonArray(emptyList()))
            }
        }
        assertFailsWith<IllegalStateException> {
            RockyClient(transport).coordinated(bytes(ByteArray(32))) { error("application failure") }
        }
        assertEquals(listOf("BEGIN", "ROLLBACK"), transport.calls.map { it.sql() })
    }
}
