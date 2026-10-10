package rocky.android

import android.app.Activity
import android.app.Application
import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import rocky.LocalDiscoveryHold
import rocky.RockyDatabase

/**
 * Follows the app's visibility: with no started activity the followed databases enter the background, with
 * the first one they return to the foreground. Transitions apply in order per database; a quick stop and
 * start ends in the latest state. It never closes a database or a network.
 *
 * While a followed database's local-network discovery is on, it holds the Wi-Fi multicast lock, and releases it
 * in the background and when the database is no longer followed. The lock needs the
 * `CHANGE_WIFI_MULTICAST_STATE` permission, which the SDK's manifest declares.
 *
 * Register it in `Application.onCreate`, before any activity starts, so it counts every one.
 */
class RockyLifecycle private constructor(private val application: Application) : Application.ActivityLifecycleCallbacks {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val followed = mutableMapOf<RockyDatabase, Channel<Boolean>>()
    private var started = 0
    private val multicast by lazy {
        (application.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager)
            .createMulticastLock("rocky-discovery")
            .apply { setReferenceCounted(true) }
    }

    /** Whether the Wi-Fi multicast lock is held for a followed database. */
    val holdsMulticastLock: Boolean get() = multicast.isHeld

    /** Whether an activity of the app is started. */
    val isForeground: Boolean @Synchronized get() = started > 0

    /** Applies visibility in order; [onApplied] runs on an IO worker after each transition completes. */
    @Synchronized
    fun follow(database: RockyDatabase, onApplied: (Boolean) -> Unit = {}) {
        if (database in followed) return
        val visible = Channel<Boolean>(Channel.CONFLATED)
        followed[database] = visible
        val hold = LocalDiscoveryHold({ multicast.acquire() }, { multicast.release() })
        scope.launch {
            try {
                for (foreground in visible) {
                    if (foreground) database.enterForeground() else database.enterBackground()
                    // Without the permission the path still announces; hearing others is then up to the radio.
                    runCatching { hold.apply(database.discovery()) }
                    onApplied(foreground)
                }
            } finally {
                runCatching { hold.apply(null) }
            }
        }
        visible.trySend(started > 0)
    }

    /** Stops following `database`; it stays in its current state. Call it before closing the database. */
    @Synchronized
    fun unfollow(database: RockyDatabase) {
        followed.remove(database)?.close()
    }

    @Synchronized
    override fun onActivityStarted(activity: Activity) {
        if (started++ == 0) followed.values.forEach { it.trySend(true) }
    }

    @Synchronized
    override fun onActivityStopped(activity: Activity) {
        if (started > 0 && --started == 0) followed.values.forEach { it.trySend(false) }
    }
    override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
    override fun onActivityResumed(activity: Activity) {}
    override fun onActivityPaused(activity: Activity) {}
    override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
    override fun onActivityDestroyed(activity: Activity) {}

    /** Stops following every database and the app's activities. */
    @Synchronized
    fun unregister() {
        application.unregisterActivityLifecycleCallbacks(this)
        followed.values.forEach { it.close() }
        followed.clear()
        scope.cancel()
    }

    companion object {
        fun register(application: Application): RockyLifecycle = RockyLifecycle(application).also { application.registerActivityLifecycleCallbacks(it) }
    }
}
