package org.example.examples.shopping

import kotlinx.datetime.TimeZone
import org.example.examples.shopping.presentation.screen.sync.ActivityEntry
import org.example.examples.shopping.presentation.screen.sync.ActivityFilter
import org.example.examples.shopping.presentation.screen.sync.ActivityFilters
import org.example.examples.shopping.presentation.screen.sync.EntryKind
import org.example.examples.shopping.presentation.screen.sync.describe
import org.example.examples.shopping.presentation.screen.sync.filterActivity
import org.example.examples.shopping.util.formatTimestamp
import rocky.RockyEvent
import kotlin.test.*

class ActivityFiltersTest {
    private val entries = listOf(
        ActivityEntry("rocky-4", 4, EntryKind.PeerDown, "Cannot reach kitchen", "deadline elapsed", isProblem = true),
        ActivityEntry("rocky-3", 3, EntryKind.PeerDown, "Link closed with kitchen", "went offline"),
        ActivityEntry("rocky-2", 2, EntryKind.Transfer, "Synced groceries", "Milk: 12 bytes received"),
        ActivityEntry("rocky-1", 1, EntryKind.Problem, "Could not renew credentials", "issuer unavailable"),
    )

    @Test fun `copyable event timestamps preserve milliseconds`() {
        assertEquals("1970-01-01 00:00:01.234", formatTimestamp(1_234, TimeZone.UTC))
    }

    @Test fun `problems includes failed connections but excludes normal link closure`() {
        assertEquals(listOf("rocky-4", "rocky-1"), filterActivity(entries, "", setOf(ActivityFilter.Problems)).map { it.key })
        assertEquals(listOf("rocky-4", "rocky-3"), filterActivity(entries, "", setOf(ActivityFilter.Connections)).map { it.key })
    }

    @Test fun `native connection errors are classified as problems while graceful closure is not`() {
        fun entry(event: RockyEvent) = describe(event, emptyMap(), emptyMap(), emptyMap())
        assertTrue(entry(RockyEvent.PeerAttemptFailed(1, 1, "node", "timeout")).isProblem)
        assertTrue(entry(RockyEvent.LinkClosed(2, 2, "node", "failure", "stream reset")).isProblem)
        assertFalse(entry(RockyEvent.LinkClosed(3, 3, "node", "background", null)).isProblem)
    }

    @Test fun `combined categories do not duplicate overlapping events or reorder the feed`() {
        assertEquals(entries, filterActivity(entries, "", setOf(ActivityFilter.Problems, ActivityFilter.Connections, ActivityFilter.Transfers)))
    }

    @Test fun `search matches every term across title and detail and intersects categories`() {
        assertEquals(listOf(entries[2]), filterActivity(entries, "  GROCERIES   milk  ", setOf(ActivityFilter.Transfers)))
        assertEquals(emptyList(), filterActivity(entries, "milk", setOf(ActivityFilter.Connections)))
        assertEquals(listOf(entries[0]), filterActivity(entries, "rocky-4"))
        assertEquals(entries, filterActivity(entries, "   "))
    }

    @Test fun `new events retain the chosen query and clearing restores the full feed`() {
        val filters = ActivityFilters()
        filters.search = "kitchen"
        filters.toggle(ActivityFilter.Problems)
        val updated = listOf(ActivityEntry("rocky-5", 5, EntryKind.Transfer, "Synced kitchen")) + entries
        assertEquals(listOf(entries[0]), filterActivity(updated, filters.search, filters.selected))
        filters.toggle(ActivityFilter.Connections)
        filters.toggle(ActivityFilter.Problems)
        assertEquals(listOf(entries[0], entries[1]), filterActivity(updated, filters.search, filters.selected))
        filters.clear()
        assertEquals(updated, filterActivity(updated, filters.search, filters.selected))
    }
}
