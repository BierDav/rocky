package rocky

import kotlinx.coroutines.*
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.json.*
import kotlin.test.*

@OptIn(ExperimentalSerializationApi::class)
class TipsTest {
    private val status = Json.parseToJsonElement(
        """{
          "bookmark":null,"tip":"tip-main","operations_since_seal":18446744073709551615,
          "evidence_releasable":null,
          "devices":[
            {"node":"peer","tip":null,"frontiers":[]},
            {"node":"other","tip":"tip-main","frontiers":[{"scope":"{\"table\":\"resource\"}",
              "logs":[{"author":"key","held":9007199254740993,"applied":5}]}]}
          ]
        }""",
    ).jsonObject
    private class Recording(private val answer: (JsonObject) -> JsonObject) : Transport {
        val calls = mutableListOf<JsonObject>()
        override suspend fun call(request: JsonObject): JsonObject {
            calls.add(request)
            return answer(request)
        }
        override suspend fun close() {}
    }

    private fun recordingTips(answer: (JsonObject) -> JsonObject = { status }): Pair<Recording, Tips> {
        val transport = Recording(answer)
        return transport to RockyClient(transport).tips
    }

    private fun JsonObject.action() = getValue("action").jsonPrimitive.content

    @Test
    fun everyActionSendsItsRequestShape() = runBlocking {
        val (transport, tips) = recordingTips {
            when (it.action()) {
                "open" -> buildJsonObject { put("tip", "branch") }
                "inspect" -> graph
                else -> status
            }
        }
        tips.status()
        tips.inspect()
        tips.seal()
        tips.seal(close = true)
        assertEquals("branch", tips.open("seal-id", 3u))
        tips.createBookmark("dev")
        tips.createBookmark("release", "tip-release")
        tips.moveBookmark("dev", "branch")
        tips.deleteBookmark("release")
        tips.follow("dev")
        tips.drop("tip-old")
        fun request(vararg fields: Pair<String, String>) = buildJsonObject {
            put("type", "tips")
            fields.forEach { (name, value) -> put(name, value) }
        }
        assertEquals(
            listOf(
                request("action" to "status"),
                request("action" to "inspect"),
                JsonObject(request("action" to "seal") + ("close" to JsonPrimitive(false))),
                JsonObject(request("action" to "seal") + ("close" to JsonPrimitive(true))),
                JsonObject(request("action" to "open", "seal" to "seal-id") + ("minimum_in_sync" to JsonPrimitive(3))),
                request("action" to "create", "name" to "dev"),
                request("action" to "create", "name" to "release", "tip" to "tip-release"),
                request("action" to "move", "name" to "dev", "tip" to "branch"),
                request("action" to "delete", "name" to "release"),
                request("action" to "follow", "name" to "dev"),
                request("action" to "drop", "tip" to "tip-old"),
            ),
            transport.calls,
        )
        assertFalse("tip" in transport.calls[5])
    }

    @Test
    fun openRefusesAZeroMinimumBeforeSending() = runBlocking {
        val (transport, tips) = recordingTips()
        assertFailsWith<IllegalArgumentException> { tips.open("seal-id", 0u) }
        assertTrue(transport.calls.isEmpty())
    }

    @Test
    fun mutationsReturnTypedStatus() = runBlocking {
        val (_, tips) = recordingTips()
        assertEquals("tip-main", tips.seal().tip)
        assertNull(tips.createBookmark("dev").bookmark)
        assertEquals(2, tips.follow("dev").devices.size)
    }

    @Test
    fun statusPreservesUnsignedCountersAndUnknownState() = runBlocking {
        val (_, tips) = recordingTips()
        val value = tips.status()
        assertNull(value.bookmark)
        assertEquals("tip-main", value.tip)
        assertEquals(ULong.MAX_VALUE, value.operationsSinceSeal)
        assertNull(value.evidenceReleasable)
        assertEquals(TipDeviceFrontier("peer", null, emptyList()), value.devices.first())
        assertEquals(
            TipDeviceFrontier(
                "other",
                "tip-main",
                listOf(TipScopeFrontier("""{"table":"resource"}""", listOf(TipLogFrontier("key", 9007199254740993uL, 5uL)))),
            ),
            value.devices.last(),
        )
        val followed = JsonObject(status + ("bookmark" to JsonPrimitive("main")))
        assertEquals("main", TipStatus.decode(followed).bookmark)
        val evidence = TipStatus.decode(
            JsonObject(
                status +
                    (
                        "evidence_releasable" to buildJsonObject {
                            put("operations", 17)
                            put("bytes", JsonUnquotedLiteral("18446744073709551615"))
                        }
                        ),
            ),
        )
        assertEquals(TipEvidence(17uL, ULong.MAX_VALUE), evidence.evidenceReleasable)
    }

    private val graph = Json.parseToJsonElement(
        """{
          "scope":"domain","genesis":"genesis","schema":"schema",
          "seals":[
            {"id":"continued","tip":"genesis","frontier":[{"author":4294967295,"sequence":18446744073709551615}],"action":"continue","schema":"schema","canonical":true},
            {"id":"loser","tip":"genesis","frontier":[],"action":"continue","schema":"schema","canonical":false},
            {"id":"closed","tip":"abandoned","frontier":[],"action":"close","schema":null,"canonical":true}
          ],
          "openings":[{"id":"branch","seal":"continued","minimum_in_sync":3}],
          "bookmarks":[{"name":"main","tip":"continued","moves":[
            {"id":"created","from":null,"to":"genesis"},{"id":"advance","from":"genesis","to":"continued"}
          ]},{"name":"deleted","tip":null,"moves":[{"id":"deleted-move","from":"branch","to":null}]}],
          "nodes":[{"node":"local","followed_tip":"continued","kept_tips":["genesis","continued"],"kept_seals":["continued"]},
            {"node":"remote","followed_tip":null,"kept_tips":null,"kept_seals":null}]
        }""",
    ).jsonObject

    @Test
    fun inspectionDecodesSealsOpeningsBookmarksAndUnknownHoldings() = runBlocking {
        val value = recordingTips { graph }.second.inspect()
        assertEquals(Triple("domain", "genesis", "schema"), Triple(value.scope, value.genesis, value.schema))
        assertEquals(
            listOf(
                TipSeal("continued", "genesis", listOf(TipAuthorMark(UInt.MAX_VALUE, ULong.MAX_VALUE)), TipSealAction.Continue("schema"), true),
                TipSeal("loser", "genesis", emptyList(), TipSealAction.Continue("schema"), false),
                TipSeal("closed", "abandoned", emptyList(), TipSealAction.Close, true),
            ),
            value.seals,
        )
        assertEquals(TipOpening("branch", "continued", 3u), value.openings.single())
        assertEquals(BookmarkMove("created", null, "genesis"), value.bookmarks.first().moves.first())
        assertEquals("continued", value.bookmarks.first().tip)
        assertEquals(TipBookmark("deleted", null, listOf(BookmarkMove("deleted-move", "branch", null))), value.bookmarks.last())
        assertEquals(TipNode("local", "continued", listOf("genesis", "continued"), listOf("continued")), value.nodes.first())
        assertEquals(TipNode("remote", null, null, null), value.nodes.last())
    }

    @Test
    fun inspectionRefusesAnUnknownSealAction() = runBlocking {
        val seal = buildJsonObject {
            put("id", "x")
            put("tip", "genesis")
            put("frontier", JsonArray(emptyList()))
            put("action", "merge")
            put("schema", JsonNull)
            put("canonical", true)
        }
        val unknown = JsonObject(graph + ("seals" to JsonArray(listOf(seal))))
        assertFailsWith<IllegalArgumentException> { recordingTips { unknown }.second.inspect() }
        Unit
    }

    @Test
    fun expectedReplicaFailuresStayTyped() = runBlocking {
        val tips = recordingTips { throw RockyException("unavailable", false, "tip is still followed") }.second
        val failure = assertFailsWith<RockyException> { tips.drop("tip-main") }
        assertEquals("unavailable", failure.code)
        assertFalse(failure.retryable)
        assertEquals("tip is still followed", failure.message)
    }
}
