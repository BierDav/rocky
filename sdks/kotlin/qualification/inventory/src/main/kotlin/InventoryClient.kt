package rocky

import kotlinx.serialization.json.*

fun handle(value: Int): JsonArray = JsonArray(List(32) { JsonPrimitive(value) })

/** Typed actions consume the same precise protocol; no full-row updates. */
class InventoryClient(val rocky: RockyClient) {
    suspend fun note(handle: JsonArray, note: String, entity: JsonObject? = null, baseline: String? = null) = rocky.mutate(handle, note.toByteArray(Charsets.UTF_8), entity, baseline)
    suspend fun stock(): Long = rocky.read(query("SELECT remaining FROM stock", maxRows = 1))
        .getValue("rows").jsonArray.single().jsonArray.single().jsonObject.getValue("value").jsonPrimitive.content.toLong()
}
