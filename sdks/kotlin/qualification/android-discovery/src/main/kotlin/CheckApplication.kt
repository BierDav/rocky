package rocky.check.discovery

import android.app.Activity
import android.app.Application
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.android.RockyLifecycle
import rocky.schema
import java.io.File

/** Opens one database whose only discovery path is the local network and logs what the lifecycle holds. */
class CheckApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val lifecycle = RockyLifecycle.register(this)
        val definition = schema("discoverycheck", 1) {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        CoroutineScope(Dispatchers.IO).launch {
            try {
                val directory = File(filesDir, "check").also { it.deleteRecursively() }
                val transport = PeerTransport.Iroh(relay = "disabled", lookup = "disabled", topic = false, local = true, bind = "0.0.0.0:0")
                val database = RockyDatabase.create(directory, definition, transport, automaticSync = false)
                lifecycle.follow(database) { foreground ->
                    val local = runBlocking { database.discovery().local }
                    Log.i(TAG, "foreground=$foreground lock=${lifecycle.holdsMulticastLock} local=${local.state} reason=${local.reason}")
                }
            } catch (error: Exception) {
                Log.e(TAG, "failed ${error.message}", error)
            }
        }
    }

    companion object {
        const val TAG = "RockyDiscoveryCheck"
    }
}

/** A started activity is what the lifecycle counts as the foreground. */
class CheckActivity : Activity()
