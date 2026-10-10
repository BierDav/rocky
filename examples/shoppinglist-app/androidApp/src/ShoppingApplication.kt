package org.example.examples

import android.app.Application
import android.os.Build
import android.util.Log
import androidx.room.Room
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingDatabase
import org.example.examples.shopping.ShoppingGraph
import org.example.examples.shopping.ShoppingLinks
import org.example.examples.shopping.ShoppingPlatform
import org.example.examples.shopping.publicCoordination
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle
import java.io.File
import java.util.UUID

/** The current installation: keys in the Keystore, sync paused while no activity is visible. */
class ShoppingApplication : Application() {
    lateinit var graph: ShoppingGraph
        private set
    private lateinit var lifecycle: RockyLifecycle
    private val databases = mutableSetOf<RockyDatabase>()

    override fun onCreate() {
        super.onCreate()
        lifecycle = RockyLifecycle.register(this)
        graph = createGraph()
    }

    private fun createGraph(): ShoppingGraph {
        val preferences = getSharedPreferences("shopping", MODE_PRIVATE)
        return ShoppingGraph(
            ShoppingPlatform(
                directory = File(filesDir, "shopping").absolutePath,
                deviceName = Build.MODEL,
                room = { path -> Room.databaseBuilder<ShoppingDatabase>(this, path) },
                secrets = KeystoreSecretStore(this, preferences.getString("secrets-alias", null) ?: KeystoreSecretStore.DEFAULT_ALIAS),
                transport = {
                    when (preferences.getString("transport", null)) {
                        // The phone dials the routes from the invitation. As an issuer it also listens, on its own
                        // loopback port, since every app on the device would otherwise claim the default one.
                        "tcp" -> PeerTransport.Tcp(listen = "127.0.0.1:0")

                        else -> PeerTransport.Iroh()
                    }
                },
                // The coordination service is reached over iroh only.
                commands = { if (preferences.getString("transport", null) == "tcp") null else publicCoordination() },
                opened = { database ->
                    synchronized(databases) { databases += database }
                    lifecycle.follow(database) { foreground -> Log.i(TAG, if (foreground) "sync foreground" else "sync background") }
                },
            ),
        )
    }

    /** Closes this installation before deleting its files and selecting fresh credentials. */
    suspend fun reset() {
        check(graph.session.state.value != SessionState.Opening) { "The household is still opening" }
        synchronized(databases) { databases.toList().also { databases.clear() } }.forEach(lifecycle::unfollow)
        try {
            graph.session.close()
            withContext(Dispatchers.IO) {
                val directory = File(filesDir, "shopping")
                val secrets = File(noBackupFilesDir, "rocky-secrets")
                check(!directory.exists() || directory.deleteRecursively()) { "Could not remove local Shopping data" }
                check(!secrets.exists() || secrets.deleteRecursively()) { "Could not remove local Shopping secrets" }
                check(getSharedPreferences("shopping", MODE_PRIVATE).edit().remove("transport").putString("secrets-alias", "rocky.shopping.${UUID.randomUUID()}").commit()) { "Could not save the new installation" }
            }
        } finally {
            graph = createGraph()
        }
    }

    /** A driver link's transport choice; it applies to the first join or create only. */
    fun chooseTransport(kind: String?) {
        getSharedPreferences("shopping", MODE_PRIVATE).edit().putString("transport", kind ?: "iroh").commit()
    }

    companion object {
        const val TAG = ShoppingLinks.TAG
    }
}
