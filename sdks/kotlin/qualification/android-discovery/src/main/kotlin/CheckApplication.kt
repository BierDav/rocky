package rocky.check.discovery

import android.app.Activity
import android.app.Application
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import rocky.GrantProfile
import rocky.LinkState
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.Schema
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
                checkLinks(definition)
                lifecycle.follow(database) { foreground ->
                    val local = runBlocking { database.discovery().local }
                    Log.i(TAG, "foreground=$foreground lock=${lifecycle.holdsMulticastLock} local=${local.state} reason=${local.reason}")
                }
            } catch (error: Exception) {
                Log.e(TAG, "failed ${error.message}", error)
            }
        }
    }

    private suspend fun checkLinks(definition: Schema) {
        val aDirectory = File(filesDir, "link-a").also { it.deleteRecursively() }
        val bDirectory = File(filesDir, "link-b").also { it.deleteRecursively() }
        val transport = PeerTransport.Iroh.isolated()
        val a = RockyDatabase.create(aDirectory, definition, transport)
        var b: RockyDatabase? = null
        try {
            b = RockyDatabase.join(bDirectory, definition, a.pairing(GrantProfile.Issuer).uri, transport)
            a.addPeer(b.contact().uri)
            withTimeout(5_000) { a.observeStatus().first { status -> status.peers.any { it.link.state == LinkState.Open } } }
            withTimeout(1_000) { a.enterBackground() }
            check(a.network().peers.all { it.link.state == LinkState.Closed }) { "background left a link open" }
            a.enterForeground()
            withTimeout(5_000) { a.observeStatus().first { status -> status.peers.any { it.link.state == LinkState.Open } } }
            Log.i(TAG, "links background=closed foreground=open")
        } finally {
            a.enterBackground()
            b?.enterBackground()
            b?.close()
            a.close()
        }
    }

    companion object {
        const val TAG = "RockyDiscoveryCheck"
    }
}

/** A started activity is what the lifecycle counts as the foreground. */
class CheckActivity : Activity()
