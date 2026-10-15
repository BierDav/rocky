package rocky

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.buffer
import kotlinx.serialization.json.*

/**
 * What a database did in the background, in order: sync transfers, checkpoints, horizons, enrollment, renewal, author
 * and group key changes. The database keeps the latest 512 in memory; see `docs/runtime/activity.md`.
 */
sealed interface RockyEvent {
    val sequence: Long
    val atEpochMillis: Long

    /** A sync with [peer] moved frames; [scopes] lists each scope that changed. */
    data class Synced(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val pulled: Int,
        val pushed: Int,
        val scopes: List<ScopeTransfer>,
    ) : RockyEvent

    /** A scope restarted from [peer]'s checkpoint, dropping [superseded] operations it covered. */
    data class CheckpointImported(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val scope: ScopeName,
        val checkpoint: String,
        val superseded: Long,
    ) : RockyEvent

    /** Every replica applied the scope's history up to its horizon, so it was pruned. */
    data class HorizonFinalized(override val sequence: Long, override val atEpochMillis: Long, val scope: ScopeName) : RockyEvent

    /** A checkpoint-first bootstrap of [scopes] scopes started or [finished] with [peer]. */
    data class Bootstrap(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val scopes: Int?,
        val finished: Boolean,
    ) : RockyEvent

    /** An authenticated link became available. */
    data class LinkOpened(override val sequence: Long, override val atEpochMillis: Long, val peer: String, val reason: String) : RockyEvent

    /** An authenticated link ended; [error] is present for a failed link. */
    data class LinkClosed(override val sequence: Long, override val atEpochMillis: Long, val peer: String, val reason: String, val error: String?) : RockyEvent

    /** [peer] answered again after an error or for the first time. */
    data class PeerReconciliationCompleted(override val sequence: Long, override val atEpochMillis: Long, val peer: String) : RockyEvent

    /** [peer] stopped answering, or failed differently than before. */
    data class PeerAttemptFailed(override val sequence: Long, override val atEpochMillis: Long, val peer: String, val error: String) : RockyEvent

    /** A sync with [peer] skipped [scopes]: one side may not hold them, or [peer] has not learned that partition yet. Empty once nothing is refused. */
    data class ScopesRefused(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val scopes: List<ScopeName>,
    ) : RockyEvent

    /** A sync pushed no history of [scopes] to [peer], which restores them from a checkpoint. Empty once nothing waits. */
    data class ScopesWaiting(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val scopes: List<ScopeName>,
    ) : RockyEvent

    data class PeerAdded(override val sequence: Long, override val atEpochMillis: Long, val peer: String) : RockyEvent
    data class PeerRemoved(override val sequence: Long, override val atEpochMillis: Long, val peer: String) : RockyEvent

    /** Credentials were renewed; the device now holds [revocations], with [keys] new reader keys. */
    data class Renewed(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val revocations: RevocationStatus,
        val expiresAtEpochSeconds: Long,
        val keys: Int,
    ) : RockyEvent
    data class RenewalFailed(override val sequence: Long, override val atEpochMillis: Long, val error: String) : RockyEvent
    data class InvitationCreated(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val profile: GrantProfile,
        val expiresAtEpochSeconds: Long,
    ) : RockyEvent

    /** This issuer enrolled [node]. */
    data class DeviceEnrolled(override val sequence: Long, override val atEpochMillis: Long, val node: String, val profile: GrantProfile) : RockyEvent
    data class DeviceRevoked(override val sequence: Long, override val atEpochMillis: Long, val node: String) : RockyEvent

    /** This device can now insert into [collection] as [author]. */
    data class AuthorInstalled(override val sequence: Long, override val atEpochMillis: Long, val collection: String, val author: Int) : RockyEvent

    /** This issuer assigned [node] an author for [collection]. */
    data class AuthorAssigned(override val sequence: Long, override val atEpochMillis: Long, val node: String, val collection: String) : RockyEvent

    /** A group partition changed here: [action] is `created`, `shared`, `key_installed`, `bootstrap`, `subscribed`, `unsubscribed` or `evicted`. */
    data class PartitionChanged(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val action: String,
        val group: String?,
        val values: List<String>,
        val detail: JsonObject,
    ) : RockyEvent
    data class NetworkingControl(override val sequence: Long, override val atEpochMillis: Long, val enabled: Boolean) : RockyEvent

    /**
     * One delivery round with [peer]: envelopes it acknowledged ([handed]), deposits it stored or [refused] as a
     * mailbox, envelopes [received] from it and [receipts] of this device's deposits it returned.
     */
    data class EnvelopesDelivered(
        override val sequence: Long,
        override val atEpochMillis: Long,
        val peer: String,
        val handed: Int,
        val deposited: Int,
        val refused: Int,
        val received: Int,
        val receipts: Int,
    ) : RockyEvent

    /** An envelope from [peer] did not install; its sender keeps it. */
    data class EnvelopeRefused(override val sequence: Long, override val atEpochMillis: Long, val peer: String, val error: String) : RockyEvent

    /** A kind this SDK does not know yet. */
    data class Other(override val sequence: Long, override val atEpochMillis: Long, val kind: String, val detail: JsonObject) : RockyEvent

    companion object {
        internal fun of(event: JsonObject): RockyEvent {
            val sequence = event.long("sequence")
            val at = event.long("at")
            fun text(name: String) = event.getValue(name).jsonPrimitive.content
            fun int(name: String) = event.getValue(name).jsonPrimitive.int
            return when (val kind = text("kind")) {
                "synced" -> Synced(
                    sequence,
                    at,
                    text("peer"),
                    int("pulled"),
                    int("pushed"),
                    event.getValue("scopes").jsonArray.map { scope ->
                        scope.jsonObject.let { ScopeTransfer(ScopeName.of(it), it.int("pulled"), it.int("pushed"), it.long("bytes")) }
                    },
                )

                "checkpoint_imported" -> CheckpointImported(sequence, at, text("peer"), ScopeName.of(event), text("checkpoint"), event.long("superseded"))

                "horizon_finalized" -> HorizonFinalized(sequence, at, ScopeName.of(event))

                "bootstrap_started" -> Bootstrap(sequence, at, text("peer"), int("scopes"), finished = false)

                "bootstrap_finished" -> Bootstrap(sequence, at, text("peer"), null, finished = true)

                "link_opened" -> LinkOpened(sequence, at, text("peer"), text("reason"))

                "link_closed" -> LinkClosed(sequence, at, text("peer"), text("reason"), event["error"]?.jsonPrimitive?.contentOrNull)

                "peer_reconciliation_completed" -> PeerReconciliationCompleted(sequence, at, text("peer"))

                "peer_attempt_failed" -> PeerAttemptFailed(sequence, at, text("peer"), text("error"))

                "scopes_refused" -> ScopesRefused(
                    sequence,
                    at,
                    text("peer"),
                    event.getValue("scopes").jsonArray.map { ScopeName.of(it.jsonObject) },
                )

                "scopes_waiting" -> ScopesWaiting(
                    sequence,
                    at,
                    text("peer"),
                    event.getValue("scopes").jsonArray.map { ScopeName.of(it.jsonObject) },
                )

                "peer_added" -> PeerAdded(sequence, at, text("peer"))

                "peer_removed" -> PeerRemoved(sequence, at, text("peer"))

                "renewed" -> Renewed(sequence, at, RockySession.revocations(event.getValue("revocations").jsonObject), event.long("expires"), int("keys"))

                "renewal_failed" -> RenewalFailed(sequence, at, text("error"))

                "invitation_created" -> InvitationCreated(sequence, at, RockySession.profile(event.getValue("profile").jsonObject), event.long("expires"))

                "device_enrolled" -> DeviceEnrolled(sequence, at, text("node"), RockySession.profile(event.getValue("profile").jsonObject))

                "device_revoked" -> DeviceRevoked(sequence, at, text("node"))

                "author_installed" -> AuthorInstalled(sequence, at, text("collection"), int("author"))

                "author_assigned" -> AuthorAssigned(sequence, at, text("node"), text("collection"))

                "networking_enabled", "networking_disabled" -> NetworkingControl(sequence, at, kind == "networking_enabled")

                "envelopes_delivered" -> EnvelopesDelivered(sequence, at, text("peer"), int("handed"), int("deposited"), int("refused"), int("received"), int("receipts"))

                "envelope_refused" -> EnvelopeRefused(sequence, at, text("peer"), text("error"))

                else -> if (kind.startsWith("partition_")) {
                    PartitionChanged(
                        sequence,
                        at,
                        kind.removePrefix("partition_"),
                        event["group"]?.jsonPrimitive?.contentOrNull,
                        (event["values"] as? JsonArray).orEmpty().map { (it as? JsonObject)?.get("value")?.jsonPrimitive?.content ?: it.toString() },
                        event,
                    )
                } else {
                    Other(sequence, at, kind, event)
                }
            }
        }
        private fun JsonObject.long(name: String) = getValue(name).jsonPrimitive.content.toLong()
        private fun JsonObject.int(name: String) = getValue(name).jsonPrimitive.int
    }
}

/** A scope as activity names it: its table and, for a partition, the id `GroupStatus` reports per table. */
data class ScopeName(val table: String?, val partition: String?) {
    internal companion object {
        fun of(value: JsonObject) = ScopeName(value["table"]?.jsonPrimitive?.contentOrNull, value["partition"]?.jsonPrimitive?.contentOrNull)
    }
}

/** Frames one scope pulled and pushed in a sync. */
data class ScopeTransfer(val scope: ScopeName, val pulled: Int, val pushed: Int, val bytes: Long)

/** The kept events, then each new one; cancelling stops only this observer. */
internal fun RockyClient.activity(): Flow<RockyEvent> = observation {
    var page = call(request("activity"))
    while (true) {
        page.getValue("events").jsonArray.forEach { emit(RockyEvent.of(it.jsonObject)) }
        page = call(request("activity_next") { put("after", page.getValue("token")) })
    }
}.buffer(Channel.BUFFERED)
