package rocky

import kotlinx.serialization.json.*

/** A format-4 definition: every table has implicit `id` and `row_kind` columns and is written with SQL. */
data class Schema(val application: String, val version: Int = 1, val tables: List<Table>, val commands: CommandSet? = null, val updatePolicy: UpdatePolicy = UpdatePolicy.Changed, val views: Map<String, String> = emptyMap()) {
    init {
        require(application.matches(IDENTIFIER) && version > 0 && tables.isNotEmpty() && tables.size <= 64)
        require(tables.map { it.name }.distinct().size == tables.size)
        validatePartitionGroups(tables)
        validateReferences(tables)
        require(tables.all { it.primaryKey.isNotEmpty() }) { "every table declares a primary key; use the schema builder" }
    }
    /** The definition the runtime installs; equivalent declarations produce equal JSON. */
    fun json(): String = buildJsonObject {
        put("application", application); put("version", version); put("format", 4)
        put("tables", JsonArray(tables.sortedBy { it.logical ?: it.name }.map { it.definition() }))
        put("update_policy", updatePolicy.wire)
        if (views.isNotEmpty()) put("views", viewsJson())
        // Part of the domain's signed definition: every device declares the same commands.
        commands?.let { put("commands", it.json()) }
    }.toString()
    /** Portable definition data, bound to the application's publisher. */
    fun definitionJson(publisher: String): String {
        require(publisher.matches(Regex("[0-9a-fA-F]{64}"))) { "publisher must be 64 hexadecimal digits" }
        return buildJsonObject {
            put("format", 4); put("publisher", publisher.lowercase()); put("application", application)
            if (views.isNotEmpty()) put("views", viewsJson())
            put("update_policy", updatePolicy.wire); put("tables", JsonArray(tables.sortedBy { it.logical ?: it.name }.map { it.definition() }))
        }.toString()
    }
    private fun viewsJson(): JsonArray = JsonArray(views.toSortedMap().map { (name, select) -> buildJsonObject { put("name", name); put("create_sql", "CREATE VIEW \"$name\" AS $select") } })
    fun table(name: String): Table = tables.firstOrNull { it.name == name } ?: error("unknown table $name")
}
internal val IDENTIFIER = Regex("[A-Za-z][A-Za-z0-9_]{0,63}")
data class Table(val name: String, val columns: List<Column>, val primaryKey: List<String> = emptyList(),
    val indexes: List<Index> = emptyList(), val tableClass: TableClass = TableClass.Mergeable,
    val logical: String? = null, val foreignKeys: List<ForeignKey> = emptyList(),
    val partitionBy: List<String> = emptyList(), internal val roomIdentityIndex: List<String>? = null,
    val existenceLog: Boolean = false, val partitionGroup: String? = null) {
    init { require(name.matches(IDENTIFIER) && columns.isNotEmpty() && columns.size <= 128); require(columns.map { it.name }.distinct().size == columns.size) }
    internal fun definition(): JsonObject = buildJsonObject {
        put("name", name); logical?.takeUnless { it == name }?.let { put("logical", it) }; put("class", tableClass.wire)
        put("columns", JsonArray(columns.map { it.definition() }))
        put("primary_key", JsonArray(primaryKey.map(::JsonPrimitive)))
        put("indexes", JsonArray(indexes.sortedBy { it.name }.map { it.json() }))
        if (foreignKeys.isNotEmpty()) put("foreign_keys", JsonArray(foreignKeys.sortedBy { it.columns.joinToString(",") }.map { it.json() }))
        if (partitionBy.isNotEmpty()) put("partition_by", JsonArray(partitionBy.map(::JsonPrimitive)))
        partitionGroup?.let { put("partition_group", it) }
        if (existenceLog) put("existence_log", true)
    }
    /** Rows in primary-key order, the `id` first, as [Row.decode] reads them. */
    fun query(pageSize: Int = 128): JsonObject =
        query("SELECT id, ${columns.joinToString { it.name }} FROM $name ORDER BY ${primaryKey.joinToString()}", maxRows = pageSize)
}

data class Column(val name: String, internal val kind: String, val nullable: Boolean,
    val default: JsonElement? = null, val collation: Collation = Collation.Binary,
    val logical: String? = null, val derived: String? = null, val localOnly: Boolean = false) {
    init { require(name.matches(IDENTIFIER)); require(kind in setOf("text", "integer", "boolean", "real", "blob")) }
    internal fun definition(): JsonObject = buildJsonObject {
        put("name", name); put("type", if (kind == "boolean") "integer" else kind); put("not_null", !nullable)
        default?.takeUnless { it is JsonNull }?.let {
            val boolean = (it as? JsonPrimitive)?.takeUnless { primitive -> primitive.isString }?.booleanOrNull
            put("default", if (boolean == null) it else JsonPrimitive(if (boolean) 1 else 0))
        }
        put("collation", collation.wire)
        logical?.takeUnless { it == name }?.let { put("logical", it) }; derived?.let { put("derived", it) }; put("local_only", localOnly)
    }
    companion object {
        fun text(name: String, nullable: Boolean = false) = Column(name, "text", nullable)
        fun boolean(name: String, nullable: Boolean = false) = Column(name, "boolean", nullable)
        fun integer(name: String, nullable: Boolean = false) = Column(name, "integer", nullable)
        fun real(name: String, nullable: Boolean = false) = Column(name, "real", nullable)
        fun blob(name: String, nullable: Boolean = false) = Column(name, "blob", nullable)
    }
}

/** Typed access to a committed row; transport encodings stay inside the SDK. */
class Row internal constructor(val id: String, private val values: Map<String, JsonObject>) {
    private fun value(column: String): JsonPrimitive = cell(column).getValue("value").jsonPrimitive
    private fun cell(column: String): JsonObject = values[column] ?: error("unknown column $column")
    fun text(column: String): String = value(column).content
    fun boolean(column: String): Boolean = value(column).content != "0"
    fun integer(column: String): Long = value(column).content.toLong()
    fun real(column: String): Double = Double.fromBits(value(column).content.toULong(16).toLong())
    fun blob(column: String): ByteArray = cell(column).getValue("value").jsonArray.map { it.jsonPrimitive.int.toByte() }.toByteArray()
    fun isNull(column: String): Boolean = cell(column).getValue("type").jsonPrimitive.content == "null"
    companion object {
        /** Rows of a `Table.query` snapshot; the first column is the row's `id`. */
        fun decode(snapshot: JsonObject): List<Row> {
            val columns = snapshot.getValue("columns").jsonArray.map { it.jsonPrimitive.content }
            return snapshot.getValue("rows").jsonArray.map { element ->
                val cells = element.jsonArray.map { it.jsonObject }
                Row(cells[0].getValue("value").jsonPrimitive.content, columns.zip(cells).toMap())
            }
        }
    }
}
