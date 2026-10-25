package org.example.examples.shopping.diagnostics

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingSession
import rocky.DiagnosticLevel
import rocky.RockyDiagnostics
import rocky.RockyEvent

/**
 * Persist only structural metadata: never event payloads, names, invitations or credentials. Rocky's own spans,
 * whose fields are hashes, counts, durations and error categories, are written as they close.
 */
fun observeDiagnostics(session: ShoppingSession, scope: CoroutineScope, write: (String, Throwable?) -> Unit): Job = scope.launch {
    runCatching { RockyDiagnostics.listen(DiagnosticLevel.DEBUG) { write("rocky ${it.render()}", null) } }
        .onFailure { write("rocky diagnostics unavailable", it) }
    coroutineContext[Job]?.invokeOnCompletion {
        runCatching { RockyDiagnostics.listen(DiagnosticLevel.ERROR, null) }
        write("rocky diagnostics dropped=${RockyDiagnostics.dropped}", null)
    }
    session.state.collectLatest { state ->
        write(
            "session=${state::class.simpleName}",
            when (state) {
                is SessionState.Failed -> state.error
                is SessionState.Revoked -> state.error
                else -> null
            },
        )
        if (state is SessionState.Open) {
            coroutineScope {
                val node = state.node
                write("node=${node.self}", null)
                launch {
                    val attempts = mutableMapOf<ULong, String>()
                    val links = mutableMapOf<String, String>()
                    node.rocky.observeStatus().map { status ->
                        val network = status.network
                        for (peer in network.peers) {
                            val path = peer.link.paths.filter { it.selected }.joinToString(",") { it.kind }
                            val signature = "${peer.link.state}:$path"
                            if (links.put(peer.node, signature) != signature) {
                                write("link peer=${peer.node} state=${peer.link.state} path=$path rttMs=${peer.link.roundTripMillis}", null)
                            }
                        }
                        links.keys.retainAll(network.peers.map { it.node }.toSet())
                        val current = network.peers.flatMap { it.attempts } + network.contactResolutions
                        for (attempt in current) {
                            val detail = "attempt id=${attempt.id} peer=${attempt.target} operation=${attempt.operation} stage=${attempt.stage} outcome=${attempt.outcome} failure=${attempt.failure?.kind}"
                            if (attempts.put(attempt.id, detail) != detail) write(detail, null)
                        }
                        attempts.keys.retainAll(current.map { it.id }.toSet())
                        "status revision=${status.data.revision} bootstrap=${status.data.bootstrapPending} blockers=${blockers(status.data.blockers)} attachment=${network.attachment} enabled=${network.networkingEnabled} foreground=${network.foreground} connected=${network.connectedPeerCount} reconciling=${network.reconciling}"
                    }.distinctUntilChanged().catch { write("status observer failed", it) }.collect { write(it, null) }
                }
                launch {
                    node.rocky.activity().catch { write("activity observer failed", it) }.collect { event ->
                        val detail = when (event) {
                            is RockyEvent.Synced -> "peer=${event.peer} pulled=${event.pulled} pushed=${event.pushed} scopes=${event.scopes.size}"
                            is RockyEvent.Bootstrap -> "peer=${event.peer} finished=${event.finished} scopes=${event.scopes}"
                            is RockyEvent.CheckpointImported -> "peer=${event.peer} checkpoint=${event.checkpoint} superseded=${event.superseded}"
                            is RockyEvent.LinkOpened -> "peer=${event.peer}"
                            is RockyEvent.LinkClosed -> "peer=${event.peer} failed=${event.error != null} failure=${failureCategory(event.error)}"
                            is RockyEvent.PeerAttemptFailed -> "peer=${event.peer} failed=true failure=${failureCategory(event.error)}"
                            is RockyEvent.EnvelopeRefused -> "peer=${event.peer} failed=true failure=${failureCategory(event.error)}"
                            is RockyEvent.PeerAdded -> "peer=${event.peer}"
                            is RockyEvent.PeerRemoved -> "peer=${event.peer}"
                            is RockyEvent.PeerReconciliationCompleted -> "peer=${event.peer}"
                            is RockyEvent.RenewalFailed -> "failed=true failure=${failureCategory(event.error)}"
                            is RockyEvent.Other -> "kind=${event.kind} failure=${failureCategory((event.detail["error"] as? JsonPrimitive)?.contentOrNull)}"
                            else -> ""
                        }
                        write("event=${event::class.simpleName} sequence=${event.sequence} at=${event.atEpochMillis} $detail", null)
                    }
                }
            }
        }
    }
}

/** Only a fixed vocabulary leaves the SDK error; arbitrary values and identifiers are omitted. */
internal fun failureCategory(error: String?): String {
    if (error == null) return "none"
    val timeouts = listOf(
        "ciphertext custody denied", "row subscription denied", "schema publication is private",
        "native read timeout", "native write timeout", "link ready timeout", "link finish timeout",
        "link read timeout", "link write timeout", "link channel read timeout", "link channel write timeout",
        "link channel close timeout", "link channel quota timeout", "link reconcile start timeout",
        "link reconcile close timeout", "link reconcile finish timeout", "link drain timeout", "link push write timeout",
    )
    timeouts.firstOrNull { error.endsWith(it) }?.let { return it.replace(' ', '_') }
    val words = error.lowercase().split(Regex("[^a-z]+")).toSet()
    return listOf(
        "rejected", "denied", "protocol", "transport", "connection", "closed", "timeout", "deadline",
        "checkpoint", "bootstrap", "scope", "holding", "subscription", "grant", "credentials", "authority",
        "schema", "tip", "source", "unknown", "missing", "invalid", "mismatch", "frontier", "proof",
        "coordinated", "entry", "evidence", "trusted", "membership", "seal", "behind", "unavailable",
        "budget", "exceeded", "session", "quota", "unexpected", "response", "frame", "signature",
    ).filter { it in words }.joinToString(",").ifEmpty { "unclassified" }
}

/** Each blocker as its scope label, kind and what clears it. */
internal fun blockers(blockers: List<rocky.Blocker>): String = blockers.joinToString(",") { "${it.scope}:${it.kind::class.simpleName}:${it.clearsBy::class.simpleName}" }.ifEmpty { "none" }
