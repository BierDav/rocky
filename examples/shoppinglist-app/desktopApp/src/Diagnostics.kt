package org.example.examples

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingSession
import org.slf4j.LoggerFactory
import rocky.RockyEvent

/** Log failures even when the Sync screen is closed. */
internal fun observeDiagnostics(session: ShoppingSession): Job {
    val logger = LoggerFactory.getLogger("shopping.network")
    return CoroutineScope(Dispatchers.IO).launch {
        session.state.collectLatest { state ->
            when (state) {
                is SessionState.Open -> state.node.rocky.activity().collect { event ->
                    when (event) {
                        is RockyEvent.PeerAttemptFailed -> logger.warn("Sync with {} failed: {}", event.peer, event.error)

                        is RockyEvent.LinkClosed -> if (event.error != null) logger.warn("Link with {} closed: {}", event.peer, event.error)

                        is RockyEvent.EnvelopeRefused -> logger.warn("Key from {} refused: {}", event.peer, event.error)

                        is RockyEvent.RenewalFailed -> logger.warn("Credential renewal failed: {}", event.error)

                        is RockyEvent.Other -> (event.detail["error"] as? JsonPrimitive)?.contentOrNull?.let {
                            logger.warn("Rocky {}: {}", event.kind, it)
                        }

                        else -> {}
                    }
                }

                is SessionState.Failed -> logger.error("Household operation failed", state.error)

                is SessionState.Revoked -> state.error?.let { logger.warn("Rejoining household failed", it) }

                else -> {}
            }
        }
    }
}
