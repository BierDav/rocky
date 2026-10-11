@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky.ios

import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import platform.Foundation.NSNotificationCenter
import platform.Foundation.NSThread
import platform.UIKit.UIApplication
import platform.UIKit.UIApplicationDidEnterBackgroundNotification
import platform.UIKit.UIApplicationState
import platform.UIKit.UIApplicationWillEnterForegroundNotification
import rocky.RockyDatabase

/** Observes application visibility. Ordered per database, with the latest pending state winning. */
class RockyLifecycle internal constructor(initialForeground: Boolean, private val center: NSNotificationCenter) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val lock = ReentrantLock()
    private val followed = mutableMapOf<RockyDatabase, Channel<Boolean>>()
    private var foreground = initialForeground
    private var registered = true
    private val observers = mutableListOf(
        center.addObserverForName(UIApplicationDidEnterBackgroundNotification, null, null) { visible(false) },
        center.addObserverForName(UIApplicationWillEnterForegroundNotification, null, null) { visible(true) },
    )

    val isForeground: Boolean get() = lock.withLock { foreground }

    /** [onApplied] and [onError] run on a worker, after each attempted transition. */
    fun follow(database: RockyDatabase, onApplied: (Boolean) -> Unit = {}, onError: (Throwable) -> Unit = { throw it }) = lock.withLock {
        check(registered) { "lifecycle observer is unregistered" }
        if (database in followed) return@withLock
        val visible = Channel<Boolean>(Channel.CONFLATED)
        followed[database] = visible
        scope.launch {
            for (state in visible) {
                try {
                    if (state) database.enterForeground() else database.enterBackground()
                    onApplied(state)
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Throwable) {
                    onError(error)
                }
            }
        }
        visible.trySend(foreground)
        Unit
    }

    fun unfollow(database: RockyDatabase) = lock.withLock {
        followed.remove(database)?.close()
        Unit
    }

    /** Apply the combined visibility of a scene-based host; inactive scenes do not imply background. */
    fun sceneVisibility(foreground: Boolean) = visible(foreground)

    private fun visible(state: Boolean) = lock.withLock {
        foreground = state
        followed.values.forEach { it.trySend(state) }
    }

    /** Removes observers and workers; leaves every database and network open. */
    fun unregister() = lock.withLock {
        if (registered) {
            registered = false
            observers.forEach { center.removeObserver(it) }
            observers.clear()
            followed.values.forEach { it.close() }
            followed.clear()
            scope.cancel()
        }
    }

    companion object {
        /** Register on the main thread during application startup. */
        fun register(): RockyLifecycle {
            check(NSThread.isMainThread) { "register lifecycle on the main thread" }
            return RockyLifecycle(UIApplication.sharedApplication.applicationState != UIApplicationState.UIApplicationStateBackground, NSNotificationCenter.defaultCenter)
        }
    }
}
