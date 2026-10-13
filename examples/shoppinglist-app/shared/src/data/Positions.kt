package org.example.examples.shopping.data

/**
 * Sort keys of list items: decimal digit strings compared as text, so placing an item between two others writes
 * one field of one row. Keys never end in `0`, which keeps room below every key.
 */
object Positions {
    /** [count] evenly spaced keys in ascending order, as the version 7 migration writes them. */
    fun spread(count: Int): List<String> = List(count) { it.toString().padStart(9, '0') + "5" }

    /** Whether a key fits between [after] and [before]: equal keys, from concurrent places, leave no room. */
    fun fits(after: String?, before: String?): Boolean = before == null || (before.isNotEmpty() && after.orEmpty() < before)

    /** A key after [after] and before [before]; `null` is the start or the end of the list. */
    fun between(after: String?, before: String?): String {
        val low = after.orEmpty()
        require(before == null || low < before) { "$after is not before $before" }
        var bounded = before
        val key = StringBuilder()
        var index = 0
        while (true) {
            val lowDigit = low.getOrNull(index)?.digitToInt() ?: 0
            val highDigit = bounded?.getOrNull(index)?.digitToInt() ?: 10
            if (highDigit - lowDigit > 1) return key.append((lowDigit + highDigit) / 2).toString()
            key.append(lowDigit)
            // Below a higher digit, the upper bound no longer constrains later digits.
            if (highDigit > lowDigit) bounded = null
            index++
        }
    }
}
