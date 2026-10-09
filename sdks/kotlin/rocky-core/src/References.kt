package rocky

/** The columns that name one incarnation: the partition columns, then `id`. */
internal val Table.identity: List<String> get() = partitionBy + listOf("id").filterNot { it in partitionBy }

/** References the native runtime installs; each other shape fails here with its reason. */
internal fun validateReferences(tables: List<Table>) {
    for (table in tables) for (key in table.foreignKeys) {
        val target = tables.firstOrNull { it.name.equals(key.table, ignoreCase = true) }
            ?: throw IllegalArgumentException("${table.name}: a reference names the unknown table ${key.table}")
        val identity = target.identity
        require(target.tableClass == TableClass.Mergeable) { "${table.name}: a reference to ${target.name} needs a mergeable parent" }
        if (target.partitionBy.isNotEmpty()) {
            // Partitioned existence logs would be needed to resolve a reference across groups.
            require(table.partitionGroup != null && table.partitionGroup == target.partitionGroup) {
                "${table.name}: a reference to partitioned ${target.name} needs the same explicit partition group; references across groups are refused"
            }
        } else if (table.partitionBy.isNotEmpty()) {
            require(target.existenceLog) { "${table.name}: a partitioned table references a table of another group only if it is unpartitioned with an existence log; ${target.name} has none" }
        }
        require(key.columns.size == identity.size) { "${table.name}: a reference to ${target.name} names one column per identity column (${identity.joinToString()})" }
        require(key.references == null || key.references == identity) {
            "${table.name}: a reference to ${target.name} must name its identity (${identity.joinToString()}); a natural-key reference is refused"
        }
        // Partition columns correspond by position, so the reference stays inside one group partition.
        require(target.partitionBy.indices.all { key.columns[it] == table.partitionBy.getOrNull(it) }) {
            "${table.name}: a reference to ${target.name} maps its partition columns by position; begin it with (${table.partitionBy.take(target.partitionBy.size).joinToString()})"
        }
    }
}
