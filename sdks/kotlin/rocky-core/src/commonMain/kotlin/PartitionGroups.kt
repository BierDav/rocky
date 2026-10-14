package rocky

internal fun validatePartitionGroups(tables: List<Table>) {
    for (table in tables) {
        require(table.partitionGroup == null || (table.partitionGroup.matches(IDENTIFIER) && table.partitionBy.isNotEmpty())) { "${table.name}: group requires a valid name and partition columns" }
    }
    for ((name, members) in tables.filter { it.partitionBy.isNotEmpty() }.groupBy { it.partitionGroup ?: it.name }) {
        val first = members.first()
        fun slots(table: Table) = table.partitionBy.map { column ->
            if (column == "id") {
                "integer" to Collation.Binary
            } else {
                table.columns.first { it.name == column }.let { (if (it.kind == "boolean") "integer" else it.kind) to it.collation }
            }
        }
        for (table in members.drop(1)) {
            require(first.partitionGroup != null && table.partitionGroup != null) { "${first.name} and ${table.name}: group $name is reserved for its ungrouped table" }
            require(slots(first) == slots(table)) { "${first.name} and ${table.name}: group $name partition slot counts, affinities and collations must match" }
        }
    }
}
