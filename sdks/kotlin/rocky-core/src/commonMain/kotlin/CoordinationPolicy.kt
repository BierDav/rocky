package rocky

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.*
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/** Persisted shared control intent. A storage adapter must commit compare-and-set before any service call. */
data class CoordinationRegistration(
    val tenant: ByteArray,
    val registration: String,
    val revision: Long,
    val control: String,
    val members: Set<String>,
    val expiresAtEpochSeconds: Long,
    val applied: Boolean,
)

/** The application/ORM supplies durable shared metadata; Rocky owns revision, renewal and retry mechanics. */
interface CoordinationRegistrationStore {
    fun observe(): Flow<CoordinationRegistration?>
    suspend fun compareAndSet(expected: CoordinationRegistration, replacement: CoordinationRegistration): Boolean
}

sealed interface CoordinationAvailability {
    data object WaitingForRegistration : CoordinationAvailability
    data class Registered(val revision: Long, val members: Int, val expiresAtEpochSeconds: Long) : CoordinationAvailability
    data class Failed(val cause: Exception) : CoordinationAvailability
}

/**
 * Applies the desired trusted identities until cancelled. Stores each control identity before sending it;
 * uncertain calls retry the same revision. The store must be shared by all devices controlling this domain.
 * Calls to the coordinator always run outside the store's transaction.
 */
suspend fun RockySession.maintainCoordination(
    store: CoordinationRegistrationStore,
    members: Flow<Set<String>>,
    lifetimeSeconds: Long = 29L * 86_400,
    renewBeforeSeconds: Long = 7L * 86_400,
    retryEvery: Duration = 30.seconds,
    onState: (CoordinationAvailability) -> Unit = {},
) {
    require(lifetimeSeconds > 0 && renewBeforeSeconds in 0 until lifetimeSeconds && retryEvery.isPositive())
    val ticks = flow {
        while (true) {
            emit(Unit)
            delay(retryEvery)
        }
    }
    var revisionRefusals = 0
    combine(store.observe(), members.distinctUntilChanged(), ticks) { stored, desired, _ -> stored to desired }
        .collectLatest { (stored, desired) ->
            if (stored == null) {
                onState(CoordinationAvailability.WaitingForRegistration)
                return@collectLatest
            }
            val now = kotlin.time.Clock.System.now().toEpochMilliseconds() / 1000
            val next = when {
                !stored.applied && stored.revision == 1L && stored.members.isEmpty() && desired.isNotEmpty() -> stored.copy(members = desired)

                stored.applied && (stored.members != desired || stored.expiresAtEpochSeconds - now < renewBeforeSeconds) -> stored.copy(
                    revision = stored.revision + 1,
                    control = newMutationHandle(),
                    members = desired,
                    expiresAtEpochSeconds = 0,
                    applied = false,
                )

                else -> stored
            }
            try {
                if (next !== stored) {
                    store.compareAndSet(stored, next)
                } else if (stored.applied) {
                    onState(CoordinationAvailability.Registered(stored.revision, stored.members.size, stored.expiresAtEpochSeconds))
                } else {
                    val owner = service(stored.tenant)
                    try {
                        val accepted = if (stored.revision == 1L) {
                            owner.register(stored.registration, stored.members.sorted(), lifetimeSeconds)
                        } else {
                            owner.control(stored.revision, stored.control, stored.members.sorted(), lifetimeSeconds)
                        }
                        revisionRefusals = 0
                        store.compareAndSet(stored, stored.copy(expiresAtEpochSeconds = accepted.expiresAtEpochSeconds, applied = true))
                    } catch (error: RockyException) {
                        // A definite revision conflict may advance; an unavailable service never changes the identity.
                        if (error.code == "rejected" && stored.revision > 1 && revisionRefusals++ < 3) {
                            store.compareAndSet(stored, stored.copy(revision = stored.revision + 1, control = newMutationHandle()))
                        }
                        throw error
                    }
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                onState(CoordinationAvailability.Failed(error))
            }
        }
}
