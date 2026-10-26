package rocky

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*

/** Reads before waiting on the token, so a registry update cannot fall between registration and the first read. */
fun Credentials.observeDevices(): Flow<List<Device>> = session.observeStatus().map { devices() }
    .retryUnavailableReads()
    .distinctUntilChangedBy { devices -> devices.map { it.copy(label = null) to it.label?.toList() } }

/** Current state of an invitation, with its durable enrollment association after use or replacement. */
data class InvitationState(val id: String, val expiresAtEpochSeconds: Long, val open: Boolean, val enrolledNode: String?)

suspend fun Credentials.invitation(uri: String): InvitationState = session.client.call(request("invitation_state") { put("uri", uri) }).let {
    InvitationState(it.getValue("id").jsonPrimitive.content, it.getValue("expires").jsonPrimitive.content.toLong(), it.getValue("open").jsonPrimitive.boolean, it["enrolled"]?.jsonPrimitive?.contentOrNull)
}

/** Event-driven state plus an exact expiry wake; no application polling or event reconstruction is needed. */
@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
fun Credentials.observeInvitation(uri: String): Flow<InvitationState> = merge(
    session.observeStatus().map { Unit },
    session.activity().filter { it is RockyEvent.InvitationCreated || it is RockyEvent.DeviceEnrolled }.map { Unit },
).conflate()
    .flatMapLatest {
        flow {
            val state = invitation(uri)
            emit(state)
            if (state.open) {
                val now = kotlin.time.Clock.System.now().toEpochMilliseconds()
                delay(((state.expiresAtEpochSeconds + 1) * 1_000 - now).coerceAtLeast(1))
                emit(invitation(uri))
            }
        }
    }.retryUnavailableReads().distinctUntilChanged()
