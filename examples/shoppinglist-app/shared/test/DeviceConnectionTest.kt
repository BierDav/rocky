package org.example.examples.shopping

import org.example.examples.shopping.presentation.screen.devices.connectionDuration
import org.example.examples.shopping.presentation.screen.devices.connectionSummary
import rocky.*
import kotlin.test.*

class DeviceConnectionTest {
    private val attempt = AttemptObservation(1u, "node", AttemptOperation.Reconcile, AttemptStage.Reconciliation, 989_000u, 990_000u, 1000u, AttemptOutcome.Completed, null)
    private val peer = Peer("node", false, true, "discovery", "selected_local", emptyList(), true, PeerLink(LinkState.Open, 900u, null, emptyList()), listOf(attempt), null, null)

    @Test fun `live timer follows the current link and resets on reconnect`() {
        assertEquals("Link open for 1m 40s", connectionSummary(peer, 1_000_000).title)
        assertEquals("Link open for 1m 41s", connectionSummary(peer, 1_001_000).title)
        val reconnected = peer.copy(link = peer.link.copy(openedAtEpochSeconds = 999u))
        assertEquals("Link open for 1s", connectionSummary(reconnected, 1_000_000).title)
        assertEquals("1d 1h 1m 1s", connectionDuration(0, 90_061_000))
        assertEquals("0s", connectionDuration(1_001, 1_000_000))
    }

    @Test fun `a successful past reconciliation does not imply an open connection`() {
        val closed = connectionSummary(peer.copy(link = PeerLink(LinkState.Closed, null, null, emptyList())), 1_000_000)
        assertEquals(LinkState.Closed, closed.state)
        assertEquals("Link closed · last Reconcile: Completed 10s ago", closed.title)
        assertNull(closed.error)
    }

    @Test fun `an old failure does not hide an open link`() {
        val failed = peer.copy(attempts = listOf(attempt.copy(outcome = AttemptOutcome.Failed, failure = AttemptFailure(FailureKind.Timeout, null, "deadline elapsed"))))
        val open = connectionSummary(failed, 1_000_000)
        assertEquals("Link open for 1m 40s", open.title)
        assertEquals("Reconciliation failed: deadline elapsed", open.error)
        val closed = connectionSummary(failed.copy(link = PeerLink(LinkState.Closed, null, null, emptyList())), 1_000_000)
        assertEquals("Link closed · last Reconcile: Failed 10s ago", closed.title)
    }
}
