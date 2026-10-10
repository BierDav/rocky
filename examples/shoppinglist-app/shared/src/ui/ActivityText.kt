package org.example.examples.shopping.ui

import rocky.GrantProfile
import rocky.RockyEvent
import rocky.ScopeName

/** How an activity line is drawn. */
enum class EntryKind { Transfer, Checkpoint, Horizon, PeerUp, PeerDown, Credentials, Device, Author, Key, Network, Problem }

/** One line of the activity log, newest first in the list. */
data class ActivityEntry(val key: String, val atEpochMillis: Long, val kind: EntryKind, val title: String, val detail: String? = null)

/**
 * Names what Rocky did in household terms. [partitions] maps a partition id, as events and `GroupStatus` name it, to
 * its list's title, [lists] a list id to its title and [devices] a node to its recorded name.
 */
fun describe(event: RockyEvent, partitions: Map<String, String>, lists: Map<Long, String>, devices: Map<String, String>): ActivityEntry {
    fun device(node: String) = devices[node] ?: "device ${node.take(6)}"
    fun scope(scope: ScopeName) = when {
        scope.partition != null -> "${scope.table} of “${partitions[scope.partition] ?: "a list"}”"
        else -> scope.table ?: "a table"
    }
    val key = "rocky-${event.sequence}"
    return when (event) {
        is RockyEvent.Synced -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Transfer,
            "Synced with ${device(event.peer)}: ${event.pulled} in, ${event.pushed} out",
            event.scopes.joinToString("\n") { "${scope(it.scope)}: ${it.pulled} in, ${it.pushed} out, ${it.bytes} bytes" },
        )

        is RockyEvent.CheckpointImported -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Checkpoint,
            "Restarted ${scope(event.scope)} from ${device(event.peer)}'s checkpoint",
            "checkpoint ${event.checkpoint.take(12)}…, ${event.superseded} operations it covers dropped",
        )

        is RockyEvent.HorizonFinalized -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Horizon,
            "Every device applied ${scope(event.scope)}: history pruned up to its horizon",
        )

        is RockyEvent.Bootstrap -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Checkpoint,
            if (event.finished) {
                "Bootstrap from ${device(event.peer)} finished"
            } else {
                "Bootstrapping ${event.scopes} scopes from ${device(event.peer)}'s checkpoints"
            },
        )

        is RockyEvent.PeerReached -> ActivityEntry(key, event.atEpochMillis, EntryKind.PeerUp, "Reached ${device(event.peer)}")

        is RockyEvent.PeerUnreachable -> ActivityEntry(key, event.atEpochMillis, EntryKind.PeerDown, "Cannot reach ${device(event.peer)}", event.error)

        is RockyEvent.ScopesRefused -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Network,
            if (event.scopes.isEmpty()) {
                "Syncing every list with ${device(event.peer)} again"
            } else {
                "Not syncing ${event.scopes.size} tables with ${device(event.peer)} until shared"
            },
            event.scopes.joinToString("\n") { scope(it) },
        )

        is RockyEvent.ResultsFailed -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Problem,
            "Could not pull approval results from ${device(event.peer)}; retrying",
            event.error,
        )

        is RockyEvent.PeerAdded -> ActivityEntry(key, event.atEpochMillis, EntryKind.Network, "Added ${device(event.peer)} as a peer")

        is RockyEvent.PeerRemoved -> ActivityEntry(key, event.atEpochMillis, EntryKind.Network, "Removed peer ${device(event.peer)}")

        is RockyEvent.Renewed -> ActivityEntry(
            key,
            event.atEpochMillis,
            EntryKind.Credentials,
            "Credentials renewed holding ${event.revocations.statements} revocations",
            "${event.keys} new reader keys",
        )

        is RockyEvent.RenewalFailed -> ActivityEntry(key, event.atEpochMillis, EntryKind.Problem, "Renewal failed", event.error)

        is RockyEvent.InvitationCreated -> ActivityEntry(key, event.atEpochMillis, EntryKind.Device, "Invitation for a ${profile(event.profile)} device created")

        is RockyEvent.DeviceEnrolled -> ActivityEntry(key, event.atEpochMillis, EntryKind.Device, "Enrolled ${device(event.node)} as ${profile(event.profile)}")

        is RockyEvent.DeviceRevoked -> ActivityEntry(key, event.atEpochMillis, EntryKind.Device, "Revoked ${device(event.node)}")

        is RockyEvent.AuthorInstalled -> ActivityEntry(key, event.atEpochMillis, EntryKind.Author, "Can write ${event.collection} as author ${event.author}")

        is RockyEvent.AuthorAssigned -> ActivityEntry(key, event.atEpochMillis, EntryKind.Author, "Assigned ${device(event.node)} an author for ${event.collection}")

        is RockyEvent.PartitionChanged -> {
            val list = event.values.firstOrNull()?.toLongOrNull()?.let(lists::get)?.let { "“$it”" } ?: "List ${event.values.joinToString()}"
            ActivityEntry(key, event.atEpochMillis, EntryKind.Key, "$list: ${PARTITION_ACTIONS[event.action] ?: event.action.replace('_', ' ')}")
        }

        is RockyEvent.Connection -> ActivityEntry(key, event.atEpochMillis, EntryKind.Network, if (event.online) "Went online" else "Went offline")

        is RockyEvent.Other -> ActivityEntry(key, event.atEpochMillis, EntryKind.Network, event.kind.replace('_', ' '))
    }
}

private val PARTITION_ACTIONS = mapOf(
    "created" to "key created",
    "shared" to "key sealed to other devices",
    "key_installed" to "key installed",
    "bootstrap" to "will start from a peer's checkpoint",
    "subscribed" to "held here",
    "unsubscribed" to "no longer followed",
    "evicted" to "evicted from this device",
)

fun profile(profile: GrantProfile): String = when (profile) {
    GrantProfile.Issuer -> "trusted"
    is GrantProfile.Writer -> "writer"
    is GrantProfile.Reader -> "reader"
    is GrantProfile.Custody -> "custody"
    is GrantProfile.Grants -> "member"
}
