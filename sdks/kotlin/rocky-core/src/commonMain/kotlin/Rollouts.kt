package rocky

import kotlinx.serialization.json.*

/** Removed table or `table.field`; rows counts live stored local rows, including NULL fields. */
data class RemovalImpact(val name: String, val rows: Long)

/** Data for the application's confirmation UI; no device inventory or user-facing wording. */
data class RolloutImpact(
    val domain: String,
    val installedManifest: String,
    val targetSchema: String,
    val tables: List<RemovalImpact>,
    val fields: List<RemovalImpact>,
    val digest: String,
) {
    internal companion object {
        fun parse(json: String): RolloutImpact {
            val report = Json.parseToJsonElement(json).jsonObject
            fun impacts(name: String): List<RemovalImpact> = report.getValue(name).jsonArray.map {
                val item = it.jsonObject
                RemovalImpact(item.getValue("name").jsonPrimitive.content, item.getValue("rows").jsonPrimitive.long)
            }
            return RolloutImpact(
                report.getValue("domain").jsonPrimitive.content,
                report.getValue("installed_manifest").jsonPrimitive.content,
                report.getValue("target_schema").jsonPrimitive.content,
                impacts("tables"),
                impacts("fields"),
                report.getValue("digest").jsonPrimitive.content,
            )
        }
    }
}

/** Open changed nothing. Show [report] and reopen with its digest only after confirmation. */
class RolloutConfirmationRequired(val report: RolloutImpact, val mismatch: Boolean) : Exception("Removal requires confirmation")
