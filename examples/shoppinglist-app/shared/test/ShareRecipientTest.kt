package org.example.examples.shopping

import org.example.examples.shopping.data.db.DeviceRow
import org.example.examples.shopping.presentation.screen.lists.discoveredRecipients
import org.example.examples.shopping.presentation.screen.lists.registryRecipients
import rocky.Device
import rocky.GrantProfile
import rocky.LinkState
import rocky.Peer
import rocky.PeerLink
import rocky.Route
import kotlin.test.*

class ShareRecipientTest {
    @Test fun `trusted sharing excludes self revoked custody and unregistered devices`() {
        val rows = listOf("self", "partial", "revoked", "custody", "unknown").map { DeviceRow(it, it, "partial") }
        val devices = listOf(
            Device("self", "user", 0, false, GrantProfile.Issuer),
            Device("partial", "user", 0, false, GrantProfile.Grants(emptyList())),
            Device("revoked", "user", 0, true, GrantProfile.Issuer),
            Device("custody", "user", 0, false, GrantProfile.Custody(emptySet())),
        )
        assertEquals(listOf("partial"), registryRecipients("self", rows, devices).map { it.node })
    }

    @Test fun `discovery supplies member sharing choices before the first connection`() {
        fun peer(node: String, source: String) = Peer(
            node = node,
            isIssuer = false,
            isConfigured = true,
            source = source,
            choice = "not_considered",
            routes = listOf(Route("iroh", "route")),
            isLocal = source == "discovery",
            state = "unknown",
            checkedAtEpochSeconds = null,
            error = null,
            link = PeerLink(LinkState.Closed, null, null),
        )
        val peers = listOf(peer("self", "discovery"), peer("nearby", "discovery"), peer("remote", "handover"), peer("nearby", "discovery"))
        val recipients = discoveredRecipients("self", peers)
        assertEquals(listOf("nearby", "remote"), recipients.map { it.node })
        assertEquals(listOf("Device nearby", "Device remote"), recipients.map { it.name })
    }
}
