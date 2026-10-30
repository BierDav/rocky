package rocky

import kotlinx.serialization.json.*

enum class TableClass(internal val wire: String) {
    Mergeable("mergeable"),

    /**
     * Replicates each transaction's statements; replicas replay them in canonical order and drop a transaction
     * whose replay fails. [RockyDatabase.transactionOutcome] reports how a committed transaction stands.
     */
    Replayed("replayed"),
    LocalOnly("local_only"),
}
enum class UpdatePolicy(internal val wire: String) { Changed("changed"), Assigned("assigned") }
enum class Collation(internal val wire: String) { Binary("binary"), Nocase("nocase"), Rtrim("rtrim") }
data class IndexColumn(val name: String, val descending: Boolean = false) {
    init {
        require(name.matches(IDENTIFIER)) { "invalid index column $name" }
    }
}

/** What a remote write that collides on a unique index does to the row with the earlier stamp. */
enum class Conflict(internal val wire: String) {
    /** Its indexed columns read NULL while the clash lasts; they must be nullable. */
    SetNull("set_null"),

    /** The whole row is hidden. */
    LatestWins("latest_wins"),
}

/** What a child shows while its parent is not alive. */
enum class Delete(internal val wire: String) {
    /** The child is hidden. */
    Cascade("cascade"),

    /** The reference reads NULL; the column must be nullable and outside the key. */
    SetNull("set_null"),
}
data class Index(val name: String, val columns: List<IndexColumn>, val unique: Boolean = false, val onConflict: Conflict? = null) {
    init {
        require(name.matches(IDENTIFIER) && columns.isNotEmpty()) { "invalid index $name" }
        require(onConflict == null || unique) { "$name: onConflict needs a unique index" }
    }
    internal fun json(): JsonObject = buildJsonObject {
        put("name", name)
        put("unique", unique)
        put(
            "columns",
            JsonArray(
                columns.map {
                    buildJsonObject {
                        put("name", it.name)
                        put("descending", it.descending)
                    }
                },
            ),
        )
        onConflict?.let { put("on_conflict", it.wire) }
    }
}

/**
 * A reference to the identity of a mergeable parent table: its partition columns, then `id`. [columns] holds one
 * child column per identity column; [references], when given (as Room declares them), must name that identity.
 */
data class ForeignKey(val columns: List<String>, val table: String, val onDelete: Delete, val references: List<String>? = null) {
    init {
        require(columns.isNotEmpty() && columns.distinct().size == columns.size && columns.all { it.matches(IDENTIFIER) } && table.matches(IDENTIFIER)) { "invalid foreign key to $table" }
        require(references == null || references.size == columns.size) { "foreign key to $table: one referenced column per column" }
    }
    internal fun json(): JsonObject = buildJsonObject {
        put("columns", JsonArray(columns.map(::JsonPrimitive)))
        put("table", table)
        put("on_delete", onDelete.wire)
    }
}

/** Builds the same portable definition whether declarations are supplied by hand or imported. */
fun schema(application: String, version: Int = 1, block: SchemaBuilder.() -> Unit): Schema = SchemaBuilder(application, version).apply(block).build()

class SchemaBuilder internal constructor(private val application: String, private val version: Int) {
    private val tables = linkedMapOf<String, TableBuilder>()
    private val views = linkedMapOf<String, String>()
    fun view(name: String, select: String) {
        require(name.matches(IDENTIFIER) && select.trimStart().startsWith("SELECT", ignoreCase = true) && ";" !in select) { "view needs a name and one SELECT" }
        require(name !in views) { "duplicate view $name" }
        views[name] = select
    }
    var updatePolicy: UpdatePolicy = UpdatePolicy.Changed
    private val migrations = mutableListOf<Migration>()

    /** Steps computing values schema [version] needs in existing rows; see [Migration]. */
    fun migration(version: Int, block: MigrationBuilder.() -> Unit) {
        migrations += Migration(version, MigrationBuilder().apply(block).steps.toList())
    }

    /** On an imported table, this adds Rocky attributes to the existing declaration. */
    fun table(name: String, block: TableBuilder.() -> Unit): TableBuilder = tables.getOrPut(name) { TableBuilder(name) }.apply(block)
    fun fromRoom(exportedJson: String) {
        val imported = importRoom(exportedJson)
        importRoomViews(exportedJson).forEach { (name, select) -> view(name, select) }
        require(imported.none { it.name in tables }) { "Room import duplicates an existing table" }
        imported.forEach { declaration ->
            table(declaration.name) {
                declaration.columns.forEach(::column)
                primaryKey(*declaration.primaryKey.toTypedArray())
                declaration.indexes.forEach(::index)
                declaration.foreignKeys.forEach(::foreignKey)
                roomIdentityIndex = declaration.roomIdentityIndex
            }
        }
    }
    internal fun build(): Schema = Schema(application, version, tables.values.map { it.build() }, updatePolicy, views, migrations.toList())
}

class TableBuilder internal constructor(val name: String) {
    private val columns = linkedMapOf<String, Column>()
    private var key: List<String> = emptyList()
    private var partitions: List<String> = emptyList()
    private var partitionGroup: String? = null
    private val indexes = linkedMapOf<String, Index>()
    private val foreignKeys = mutableListOf<ForeignKey>()
    var tableClass: TableClass = TableClass.Mergeable

    /**
     * Every device of the domain may read this table's row creations and deletions (never values),
     * so a table in another reader scope may reference it. The key must be `id`; see secure SQL.
     */
    var existenceLog: Boolean = false

    /**
     * Grants naming [table] also cover this table, for devices enrolled before it existed. Declare it only when
     * adding the table; see `docs/runtime/generations.md#tables-granted-like-another`.
     */
    var grantedLike: String? = null
    internal var roomIdentityIndex: List<String>? = null
    fun column(column: Column) {
        require(column.name !in setOf("id", "row_kind")) { "id and row_kind are implicit identity columns" }
        require(column.name !in columns) { "$name: duplicate column ${column.name}" }
        columns[column.name] = column
    }

    /** A [default] fills rows that never held the column, as when an upgrade requires it. */
    fun text(name: String, nullable: Boolean = false, default: String? = null) = column(Column.text(name, nullable).copy(default = default?.let(::JsonPrimitive)))
    fun integer(name: String, nullable: Boolean = false, default: Long? = null) = column(Column.integer(name, nullable).copy(default = default?.let(::JsonPrimitive)))
    fun boolean(name: String, nullable: Boolean = false, default: Boolean? = null) = column(Column.boolean(name, nullable).copy(default = default?.let(::JsonPrimitive)))
    fun real(name: String, nullable: Boolean = false, default: Double? = null) = column(Column.real(name, nullable).copy(default = default?.let(::JsonPrimitive)))
    fun blob(name: String, nullable: Boolean = false) = column(Column.blob(name, nullable))
    fun primaryKey(vararg columns: String) {
        key = columns.toList()
    }
    fun partitionBy(vararg columns: String, group: String? = null) {
        require(group == null || group.matches(IDENTIFIER)) { "$name: invalid partition group $group" }
        partitionGroup = group
        require(columns.isNotEmpty() && columns.size <= 16 && columns.distinct().size == columns.size) { "$name: declare 1 to 16 distinct partition columns" }
        partitions = columns.toList()
    }
    fun index(name: String, vararg columns: String) = index(name, columns.map { IndexColumn(it) })
    fun index(name: String, columns: List<IndexColumn>) = index(Index(name, columns))
    fun index(index: Index) {
        require(index.name !in indexes) { "${this.name}: duplicate index ${index.name}" }
        indexes[index.name] = index
    }

    /** A unique index of real rows; Room's default name unless [name] is given. */
    fun unique(vararg columns: String, onConflict: Conflict, name: String = "index_${this.name}_${columns.joinToString("_")}") = index(Index(name, columns.map { IndexColumn(it) }, unique = true, onConflict = onConflict))

    /** Adds Rocky's conflict policy to a unique index, such as one Room's export declared. */
    fun onConflict(index: String, policy: Conflict) {
        val old = indexes[index] ?: error("${this.name}: unknown index $index")
        indexes[index] = old.copy(onConflict = policy)
    }

    /** A reference to [table]'s `id`; declares the integer [column] unless it exists. */
    fun reference(column: String, table: String, onDelete: Delete) = reference(listOf(column), table, onDelete)
    fun reference(column: String, table: TableBuilder, onDelete: Delete) = reference(column, table.name, onDelete)

    /**
     * A reference to [table]'s identity: one column per partition column of [table], in its `partitionBy` order,
     * then the one holding its `id`. Declares each absent column as an integer, nullable under [Delete.SetNull].
     */
    fun reference(columns: List<String>, table: String, onDelete: Delete) {
        for (column in columns) if (column !in this.columns) integer(column, nullable = onDelete == Delete.SetNull)
        foreignKey(ForeignKey(columns, table, onDelete))
    }
    fun reference(columns: List<String>, table: TableBuilder, onDelete: Delete) = reference(columns, table.name, onDelete)
    fun foreignKey(key: ForeignKey) {
        require(foreignKeys.none { it.columns == key.columns }) { "$name: duplicate foreign key ${key.columns}" }
        foreignKeys += key
    }

    /** Add attributes without repeating an imported column's type. */
    fun configureColumn(name: String, block: (Column) -> Column) {
        val old = columns[name] ?: error("${this.name}: unknown column $name")
        val updated = block(old)
        require(updated.name == name) { "${this.name}: configureColumn cannot rename a column" }
        columns[name] = updated
    }
    internal fun build(policies: Boolean = true): Table {
        require(key.isNotEmpty() && key.distinct().size == key.size) { "$name: declare a non-empty primary key" }
        require(key.all { it == "id" || columns[it]?.nullable == false }) { "$name: key columns must exist and be non-null" }
        require(indexes.values.all { it.columns.isNotEmpty() && it.columns.all { term -> term.name in columns || term.name in setOf("id", "row_kind") } }) { "$name: index names an absent column" }
        if (policies && tableClass == TableClass.Mergeable) {
            indexes.values.firstOrNull { it.unique && it.onConflict == null }?.let {
                error("$name: unique index ${it.name} needs onConflict (Conflict.SetNull or Conflict.LatestWins)")
            }
        }
        if (policies) {
            for (index in indexes.values.filter { it.unique && it.onConflict != null }) {
                require(tableClass == TableClass.Mergeable && index.columns.all { it.name in columns }) { "$name: conflict policy needs mergeable application columns" }
                if (index.onConflict == Conflict.SetNull) require(index.columns.all { columns[it.name]?.nullable == true && it.name !in key }) { "$name: SetNull index columns must be nullable and outside the key" }
                // Rows clash only within one partition; uniqueness across partitions needs a coordinated write.
                if (partitions.isNotEmpty()) {
                    require(index.onConflict == Conflict.LatestWins) { "$name: unique index ${index.name} on a partitioned table takes Conflict.LatestWins; Conflict.SetNull is refused" }
                    require(partitions.all { column -> index.columns.any { it.name == column } }) { "$name: unique index ${index.name} must include every partition column (${partitions.joinToString()}); uniqueness across partitions is refused" }
                }
            }
            for (reference in foreignKeys) {
                require(tableClass == TableClass.Mergeable) { "$name: a reference needs a mergeable table" }
                for (column in reference.columns) {
                    require(column in partitions || columns[column]?.kind == "integer") { "$name.$column: a reference holds an integer id unless the column is a partition column" }
                }
                if (reference.onDelete == Delete.SetNull) {
                    val nulled = reference.columns.filterNot { it in partitions }
                    require(nulled.isNotEmpty()) { "$name: a SetNull reference needs a column outside the partition columns" }
                    require(nulled.all { columns[it]?.nullable == true && it !in key }) { "$name: SetNull reference columns must be nullable and outside the key" }
                }
            }
        }
        require(foreignKeys.all { key -> key.columns.all { it in columns } }) { "$name: a foreign key names an absent column" }
        require(partitions.all { it in key && (it == "id" || columns[it]?.let { column -> !column.nullable && !column.localOnly && column.derived == null } == true) }) { "$name: partition columns must be non-null primary-key columns" }
        // Puppets copy only the id of a referenced row.
        require(!existenceLog || (tableClass == TableClass.Mergeable && partitions.isEmpty() && key == listOf("id"))) { "$name: an existence log needs an unpartitioned mergeable table keyed by id" }
        val identity = partitions + listOf("id").filterNot { it in partitions }
        require(roomIdentityIndex == null || roomIdentityIndex == identity) { "$name: Room identity index must contain partition columns followed by id" }
        return Table(name, columns.values.toList(), key, indexes.values.toList(), tableClass, foreignKeys.toList(), partitions, existenceLog = existenceLog, partitionGroup = partitionGroup, grantedLike = grantedLike)
    }
}
