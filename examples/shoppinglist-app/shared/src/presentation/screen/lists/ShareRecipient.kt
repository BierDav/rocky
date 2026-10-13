package org.example.examples.shopping.presentation.screen.lists

import org.example.examples.shopping.data.db.DeviceRow
import rocky.Device
import rocky.GrantProfile
import rocky.Peer

/** A local sharing choice; members never read the issuer registry. */
data class ShareRecipient(val node: String, val name: String)

internal fun registryRecipients(self: String, rows: List<DeviceRow>, devices: List<Device>): List<ShareRecipient> = rows
    .filter { row -> row.node != self && devices.any { it.node == row.node && !it.revoked && it.profile !is GrantProfile.Custody } }
    .distinctBy { it.node }
    .map { ShareRecipient(it.node, it.name) }

internal fun discoveredRecipients(self: String, peers: List<Peer>): List<ShareRecipient> = peers
    .filter { it.node != self }
    .distinctBy { it.node }
    .map { ShareRecipient(it.node, "Device ${it.node.take(6)}") }
