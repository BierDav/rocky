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
    fun compactSendsKeepAndDecodesTheReport() = runBlocking {
        val report = Json.parseToJsonElement(
            """{"finalized":"second","compacted":"first","uncompacted":["second"],"waiting":null,
              "rebootstrap":null,"pruned":18446744073709551615,
              "blocked":[{"scope":"{\"table\":\"items\"}","reason":"NoPolicy"}]}""",
        ).jsonObject
        val (transport, tips) = recordingTips { report }
        assertEquals(
            TipCompaction("second", "first", listOf("second"), null, null, ULong.MAX_VALUE, listOf(TipPruneBlock("""{"table":"items"}""", "NoPolicy"))),
            tips.compact(keep = 1u),
        )
        tips.compaction()
        assertEquals(JsonPrimitive(1), transport.calls.first().getValue("keep"))
        assertEquals(listOf("compact", "compaction"), transport.calls.map { it.action() })
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
          "scope":"domain","genesis":"genesis","schema":"schema","genesis_minimum":2,
          "seals":[
            {"id":"continued","tip":"genesis","frontier":[{"author":4294967295,"sequence":18446744073709551615}],"action":"continue","schema":"schema","canonical":true,"authority":"owner","revoked":false,"final":true,"checkpoints":[{"id":"record","scope":"items","checkpoint":"body","signer":"node","authority":"issuer","rows":7}]},
            {"id":"loser","tip":"genesis","frontier":[],"action":"continue","schema":"schema","canonical":false,"authority":"issuer","revoked":true,"final":false,"checkpoints":[]},
            {"id":"closed","tip":"abandoned","frontier":[],"action":"close","schema":null,"canonical":true,"authority":"trusted","revoked":false,"final":false,"checkpoints":[]}
          ],
          "openings":[{"id":"branch","seal":"continued","minimum_in_sync":3}],
          "bookmarks":[{"name":"main","tip":"continued","moves":[
            {"id":"created","from":null,"to":"genesis","authority":"owner","revoked":false},
            {"id":"advance","from":"genesis","to":"continued","authority":"issuer","revoked":false},
            {"id":"lost","from":"genesis","to":"other","authority":"issuer","revoked":false}
          ],"losers":[{"name":"main/1","move":"lost","tip":"other"}]},
          {"name":"deleted","tip":null,"moves":[{"id":"deleted-move","from":"branch","to":null,"authority":"trusted","revoked":false}],"losers":[]}],
          "nodes":[{"node":"local","followed_tip":"continued","kept_tips":["genesis","continued"],"kept_seals":["continued"]},
            {"node":"remote","followed_tip":null,"kept_tips":null,"kept_seals":null}]
        }""",
    ).jsonObject

    @Test
    fun inspectionDecodesSealsOpeningsBookmarksAndUnknownHoldings() = runBlocking {
        val value = recordingTips { graph }.second.inspect()
        assertEquals(Triple("domain", "genesis", "schema"), Triple(value.scope, value.genesis, value.schema))
        assertEquals(2u, value.genesisMinimum)
        assertEquals(
            listOf(
                TipSeal(
                    "continued",
                    "genesis",
                    listOf(TipAuthorMark(UInt.MAX_VALUE, ULong.MAX_VALUE)),
                    TipSealAction.Continue("schema"),
                    true,
                    TipAuthority.OWNER,
                    false,
                    true,
                    listOf(TipCheckpoint("record", "items", "body", "node", TipAuthority.ISSUER, 7uL)),
                ),
                TipSeal("loser", "genesis", emptyList(), TipSealAction.Continue("schema"), false, TipAuthority.ISSUER, true, false, emptyList()),
                TipSeal("closed", "abandoned", emptyList(), TipSealAction.Close, true, TipAuthority.TRUSTED, false, false, emptyList()),
            ),
            value.seals,
        )
        assertEquals(TipOpening("branch", "continued", 3u), value.openings.single())
        assertEquals(BookmarkMove("created", null, "genesis", TipAuthority.OWNER, false), value.bookmarks.first().moves.first())
        assertEquals(listOf(LosingMove("main/1", "lost", "other")), value.bookmarks.first().losers)
        assertEquals("continued", value.bookmarks.first().tip)
        assertEquals(TipBookmark("deleted", null, listOf(BookmarkMove("deleted-move", "branch", null, TipAuthority.TRUSTED, false)), emptyList()), value.bookmarks.last())
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
            put("authority", "owner")
            put("revoked", false)
            put("final", false)
            put("checkpoints", JsonArray(emptyList()))
        }
        val unknown = JsonObject(graph + ("seals" to JsonArray(listOf(seal))))
        assertFailsWith<IllegalArgumentException> { recordingTips { unknown }.second.inspect() }
        val stranger = JsonObject(graph + ("seals" to JsonArray(listOf(JsonObject(seal + ("action" to JsonPrimitive("close")) + ("authority" to JsonPrimitive("stranger")))))))
        assertFailsWith<IllegalArgumentException> { recordingTips { stranger }.second.inspect() }
        Unit
    }

    @Test
    fun trustRequestsAndGrantDecode() = runBlocking {
        val grant = buildJsonObject {
            put("identity", "identity-key")
            put("capability", "capability-id")
            put("grant", "00ff")
        }
        val (transport, tips) = recordingTips {
            when (it.getValue("type").jsonPrimitive.content) {
                "revoke" -> buildJsonObject {}
                else -> if (it.action() == "trust") grant else status
            }
        }
        assertEquals(TrustGrant("identity-key", "capability-id", "00ff"), tips.trust("identity-key"))
        tips.installTrust("00ff")
        tips.revokeTrust("capability-id")
        assertEquals(
            listOf(
                buildJsonObject {
                    put("type", "tips")
                    put("action", "trust")
                    put("identity", "identity-key")
                },
                buildJsonObject {
                    put("type", "tips")
                    put("action", "install_trust")
                    put("grant", "00ff")
                },
                buildJsonObject {
                    put("type", "revoke")
                    put("trust", "capability-id")
                },
            ),
            transport.calls,
        )
    }

    @Test
    fun coordinatedRefusalsKeepTheirCodes() = runBlocking {
        for ((code, retryable) in listOf("sealed" to false, "coordinator_unavailable" to true)) {
            val tips = recordingTips { throw RockyException(code, retryable, "refused") }.second
            val failure = assertFailsWith<RockyException> { tips.seal() }
            assertEquals(code, failure.code)
            assertEquals(retryable, failure.retryable)
            assertFailsWith<RockyException> { tips.moveBookmark("main", "tip-old") }
        }
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
