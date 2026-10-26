package rocky

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/** Application history policy; a null threshold leaves sealing to another device. */
data class HistoryPolicy(
    val kept: KeptSeals = KeptSeals(),
    val retentionWindows: Map<String, Long> = emptyMap(),
    val sealBookmark: String? = null,
    val sealAfter: ULong? = null,
    val uncompactedSeals: UInt = 1u,
) {
    init {
        require(retentionWindows.values.all { it >= 0 } && sealAfter != 0uL)
    }
}

data class HistoryMaintenance(val sealed: String?, val compaction: TipCompaction)

suspend fun RockySession.applyHistoryPolicy(policy: HistoryPolicy) {
    if (tips.kept() != policy.kept) tips.keep(policy.kept)
    for ((table, window) in policy.retentionWindows) {
        val current = retention(table).policy
        if (current.window != window) retention(table, current.copy(window = window))
    }
}

/** One round; pending branch switches are driven durably by native recovery. */
suspend fun RockySession.maintainHistory(policy: HistoryPolicy): HistoryMaintenance {
    applyHistoryPolicy(policy)
    val status = tips.status()
    var sealed: String? = null
    if (policy.sealAfter != null && status.bookmark == policy.sealBookmark && status.operationsSinceSeal >= policy.sealAfter) {
        try {
            sealed = tips.seal().tip
        } catch (error: RockyException) {
            if (error.code !in setOf("sealed", "coordinator_unavailable")) throw error
        }
    }
    return HistoryMaintenance(sealed, tips.compact(policy.uncompactedSeals))
}

/** Runs policy until cancelled, reporting failures and retrying without ending the installation. */
suspend fun RockySession.runHistoryMaintenance(
    policy: HistoryPolicy,
    every: Duration = 30.seconds,
    onResult: suspend (HistoryMaintenance) -> Unit = {},
    onFailure: (Exception) -> Unit = {},
) {
    require(every.isPositive())
    while (true) {
        try {
            onResult(maintainHistory(policy))
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            onFailure(error)
        }
        delay(every)
    }
}
