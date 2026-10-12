package org.example.examples.qualification

import android.app.Activity
import android.app.Application
import android.net.TrafficStats
import android.os.Bundle
import android.os.Process
import android.util.Log
import android.widget.TextView
import androidx.room.Room
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.session.ShoppingPlatform
import org.example.examples.shopping.qualification.LiveLinkAgent
import org.example.examples.shopping.qualification.epochMicros
import rocky.PeerTransport
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle
import java.io.File

private const val AGENT_PORT = 47110
private const val TAG = "RockyLiveLinks"

/**
 * Serves the live-link qualification agent on loopback, reached through `adb forward`. Its households live under
 * `files/live-links` and follow this app's activities. See docs/runtime/shopping-qualification.md.
 */
class AgentApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val lifecycle = RockyLifecycle.register(this)
        val agent = LiveLinkAgent(
            platform = { directory, name, transport ->
                ShoppingPlatform(
                    directory = File(filesDir, "live-links/$directory").absolutePath,
                    deviceName = name,
                    room = { path -> Room.databaseBuilder<ShoppingDatabase>(this, path) },
                    // One sealed-secret directory and Keystore key per household, so a later run never meets an earlier one's.
                    secrets = { KeystoreSecretStore(File(noBackupFilesDir, "live-links-secrets-$directory"), "rocky.live-links.$directory") },
                    opened = { database ->
                        lifecycle.follow(database) { foreground -> Log.i(TAG, "lifecycle foreground=$foreground at=${epochMicros()}") }
                    },
                    closed = lifecycle::unfollow,
                    transport = {
                        PeerTransport.Iroh(
                            lookup = transport["lookup"]?.jsonPrimitive?.content ?: "default",
                            topic = transport["topic"]?.jsonPrimitive?.boolean ?: true,
                            local = transport["local"]?.jsonPrimitive?.boolean ?: true,
                        )
                    },
                )
            },
            log = { Log.i(TAG, it) },
            traffic = {
                val uid = Process.myUid()
                buildJsonObject {
                    put("at", epochMicros())
                    put("rxBytes", TrafficStats.getUidRxBytes(uid))
                    put("txBytes", TrafficStats.getUidTxBytes(uid))
                    put("rxPackets", TrafficStats.getUidRxPackets(uid))
                    put("txPackets", TrafficStats.getUidTxPackets(uid))
                }
            },
        )
        Thread { agent.serve(AGENT_PORT) }.start()
    }
}

/** The activity that brings the agent to the foreground; the driver leaves it with HOME and returns with `am start`. */
class AgentActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(TextView(this).apply { text = "Rocky live-link agent on loopback:$AGENT_PORT" })
    }
}
