package org.example.examples.shopping

import org.example.examples.shopping.presentation.screen.devices.connectionDuration
import org.example.examples.shopping.presentation.screen.devices.connectionSummary
import rocky.LinkState
import rocky.Peer
import rocky.PeerLink
import kotlin.test.*

class DeviceConnectionTest {
    private val peer = Peer("node", false, true, "discovery", "selected_local", emptyList(), true, "connected", 990, null, PeerLink(LinkState.Open, 900, null))

    @Test fun `live timer follows the current link and resets on reconnect`() {
        assertEquals("Live for 1m 40s", connectionSummary(peer, 1_000_000).title)
        assertEquals("Live for 1m 41s", connectionSummary(peer, 1_001_000).title)
        val reconnected = peer.copy(link = peer.link.copy(openedAtEpochSeconds = 999))
        assertEquals("Live for 1s", connectionSummary(reconnected, 1_000_000).title)
        assertEquals("1d 1h 1m 1s", connectionDuration(0, 90_061_000))
        assertEquals("0s", connectionDuration(1_001, 1_000_000))
    }

    @Test fun `a successful past sync does not imply an open connection`() {
        val closed = connectionSummary(peer.copy(link = PeerLink(LinkState.Closed, null, null)), 1_000_000)
        assertEquals(LinkState.Closed, closed.state)
        assertEquals("Link closed · last reached 10s ago", closed.title)
        assertNull(closed.error)
    }

    @Test fun `failure text is separate from status and an old failure does not hide a live link`() {
        val failed = peer.copy(state = "unreachable", error = "deadline elapsed")
        val live = connectionSummary(failed, 1_000_000)
        assertEquals("Live for 1m 40s", live.title)
        assertEquals("Last attempt failed 10s ago: deadline elapsed", live.error)
        val closed = connectionSummary(failed.copy(link = PeerLink(LinkState.Closed, null, null)), 1_000_000)
        assertEquals("Unreachable · checked 10s ago", closed.title)
        assertEquals("Last error: deadline elapsed", closed.error)
    }
}
