package rocky

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.collectLatest
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/** Desired key recipients; permissions and replica holding are configured independently. */
data class PartitionRecipients(val partition: GroupPartition, val nodes: Set<String>)

/**
 * Reconciles application policy until cancelled. Policy should derive from durable application state;
 * Rocky persists envelopes before sending and resumes their delivery after restart. Failed passes retry,
 * including when the desired state has not changed. Removing a recipient cannot retract a key already shared.
 */
suspend fun Partitions.shareFollowing(
    desired: Flow<List<PartitionRecipients>>,
    retryEvery: Duration = 15.seconds,
    onFailure: (Exception) -> Unit = {},
) {
    require(retryEvery.isPositive())
    desired.collectLatest { partitions ->
        while (true) {
            for ((partition, nodes) in partitions) {
                try {
                    val status = partition.status()
                    val epoch = status.current ?: continue
                    val missing = nodes.filter { node -> status.recipients.none { it.node == node && it.epoch == epoch } }
                    partition.share(missing)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    onFailure(error)
                }
            }
            delay(retryEvery)
        }
    }
}
