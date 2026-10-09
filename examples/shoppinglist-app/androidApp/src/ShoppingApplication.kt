package org.example.examples

import android.app.Application
import android.os.Build
import android.util.Log
import androidx.room.Room
import java.io.File
import org.example.examples.shopping.ShoppingDatabase
import org.example.examples.shopping.ShoppingGraph
import org.example.examples.shopping.ShoppingPlatform
import rocky.PeerTransport
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle

/** Builds the app's graph once: keys in the Keystore, sync paused while no activity is visible. */
class ShoppingApplication : Application() {
    lateinit var graph: ShoppingGraph
        private set

    override fun onCreate() {
        super.onCreate()
        // QRKit's scanner finds the context here; its own application class is replaced in the manifest.
        qrgenerator.AppContext.set(applicationContext)
        val lifecycle = RockyLifecycle.register(this)
        val preferences = getSharedPreferences("shopping", MODE_PRIVATE)
        graph = ShoppingGraph(ShoppingPlatform(
            directory = File(filesDir, "shopping"),
            deviceName = Build.MODEL,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(this, path) },
            secrets = KeystoreSecretStore(this),
            transport = {
                when (preferences.getString("transport", null)) {
                    // The phone dials the routes from the invitation. As an issuer it also listens, on its own
                    // loopback port, since every app on the device would otherwise claim the default one.
                    "tcp" -> PeerTransport.Tcp(listen = "127.0.0.1:0")
                    else -> PeerTransport.Iroh()
                }
            },
            opened = { database ->
                lifecycle.follow(database) { foreground -> Log.i(TAG, if (foreground) "sync foreground" else "sync background") }
            },
        ))
    }

    /** A driver's transport choice; it applies to the first join or create only. */
    fun chooseTransport(kind: String?) {
        getSharedPreferences("shopping", MODE_PRIVATE).edit().putString("transport", kind ?: "iroh").commit()
    }

    companion object {
        const val TAG = "RockyShopping"
    }
}
