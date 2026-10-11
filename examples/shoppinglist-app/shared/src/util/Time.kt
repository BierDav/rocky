package org.example.examples.shopping.util

/** How long ago [atEpochMillis] was, coarsely. */
internal fun ago(atEpochMillis: Long?, now: Long = epochMillis()): String {
    if (atEpochMillis == null) return "never"
    val seconds = (now - atEpochMillis) / 1000
    return when {
        seconds < 2 -> "just now"
        seconds < 60 -> "${seconds}s ago"
        seconds < 3600 -> "${seconds / 60}m ago"
        seconds < 86_400 -> "${seconds / 3600}h ago"
        else -> "${seconds / 86_400}d ago"
    }
}

/** A local date, such as a deadline or an expiry. */
internal fun date(epochSeconds: Long): String = formatDate(epochSeconds)
