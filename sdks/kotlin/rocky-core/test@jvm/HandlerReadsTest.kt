package rocky

import kotlinx.serialization.json.*
import kotlin.test.Test

class HandlerReadsTest {
    /** The handler frame has no tables; scalar encoding stays exact on selected reads. */
    @Test
    fun scalarsStayExactOnSelectedReads() {
        var gets = 0
        var scans = 0
        val reads = JsonCommandReads({ table, key ->
            check(table == "stock")
            val values = Json.parseToJsonElement(key).jsonArray
            check(values[0].jsonObject["value"]?.jsonPrimitive?.content == Long.MAX_VALUE.toString())
            check(values[1].jsonObject["value"]?.jsonArray == JsonArray(listOf(JsonPrimitive(0), JsonPrimitive(255))))
            gets++
            """[{"type":"integer","value":"9223372036854775807"},{"type":"blob","value":[0,255]}]"""
        }, { table, lower, upper, limit ->
            check(table == "stock" && lower != null && upper == null && limit == 1)
            scans++
            """[[{"type":"text","value":"selected"}]]"""
        })
        val command = Command("Probe", handler = "probe.v1", arguments = Shape.none,
            accepted = Shape.none, rejected = Shape.none, invocation = "stock", result = "stock", resources = listOf("stock"))
        val dispatcher = CommandDispatcher(listOf(command handledBy { context, _ ->
            val row = context.get("stock", Long.MAX_VALUE, byteArrayOf(0, -1))!!
            check(row[0] == Long.MAX_VALUE && (row[1] as ByteArray).contentEquals(byteArrayOf(0, -1)))
            check(context.scan("stock", lower = listOf("selected"), limit = 1) == listOf(listOf("selected")))
            check(runCatching { context.scan("stock", limit = 0) }.isFailure)
            Decision.accept(Unit)
        }))
        val frame = """{"version":2,"method":"Probe","command_version":1,"request":{"node":"node","sequence":"1"},"caller":"caller","predecessor":{"sequence":"0"},"arguments":[]}"""
        check(Json.parseToJsonElement(dispatcher.handle(frame, reads)).jsonObject["outcome"]?.jsonPrimitive?.content == "accepted")
        check(gets == 1 && scans == 1)
        check(runCatching { dispatcher.handle(frame.replace("\"version\":2", "\"version\":1"), reads) }.isFailure)
        check(gets == 1 && scans == 1)
    }
}
