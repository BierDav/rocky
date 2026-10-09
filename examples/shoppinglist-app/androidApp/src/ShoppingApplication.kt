package org.example.examples

import android.app.Application
import android.util.Log
import java.io.File
import kotlinx.coroutines.*
import org.example.examples.shopping.*
import rocky.*
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle
import rocky.room.followRocky

/** Room's DAOs on Rocky's file, with keys in the Keystore; Android ends the process and its jobs together. */
class Shopping(val runtime: RockyDatabase, val database: ShoppingDatabase) {
    val repository = ShoppingRepository(database, runtime)
}

class ShoppingApplication : Application() {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var lifecycle: RockyLifecycle
    private var opening: Deferred<Shopping>? = null

    override fun onCreate() {
        super.onCreate()
        lifecycle = RockyLifecycle.register(this)
    }

    /** `invitation` and `transport` apply only before the first open; later opens resume the same domain. */
    fun shopping(invitation: String? = null, transport: String? = null): Deferred<Shopping> {
        opening?.let { return it }
        val directory = File(filesDir, "shopping")
        val preferences = getSharedPreferences("shopping", MODE_PRIVATE)
        val secrets = KeystoreSecretStore(this)
        return scope.async(Dispatchers.IO) {
            val runtime = if (File(directory, "identity.json").exists()) {
                RockyDatabase.resume(directory, ShoppingSchema, peers(preferences.getString("transport", null)), secrets)
            } else {
                preferences.edit().putString("transport", transport ?: "iroh").commit()
                (if (invitation != null) RockyDatabase.join(directory, ShoppingSchema, invitation, peers(transport), secrets)
                else RockyDatabase.create(directory, ShoppingSchema, peers(transport), secrets)).also { created ->
                    if (created.isIssuer) created.installShoppingAuthor()
                }
            }
            val database = openShoppingDatabase(this@ShoppingApplication, directory, runtime)
            scope.launch(Dispatchers.IO) { database.followRocky(runtime) }
            lifecycle.follow(runtime) { foreground ->
                Log.i(MainActivity.TAG, if (foreground) "sync foreground" else "sync background")
            }
            Shopping(runtime, database)
        }.also { opening = it }
    }

    private fun peers(kind: String?): PeerTransport = when (kind) {
        // The phone dials the routes from the invitation. As an issuer it also listens, on its own
        // loopback port, since every app on the device would otherwise claim the default one.
        "tcp" -> PeerTransport.Tcp(listen = "127.0.0.1:0")
        else -> PeerTransport.Iroh()
    }
}
