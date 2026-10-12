package org.example.examples

import android.app.Application
import android.content.pm.ApplicationInfo
import android.net.TrafficStats
import android.os.Process
import android.util.Log
import androidx.room.Room
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.qualification.LiveLinkAgent
import org.example.examples.shopping.data.qualification.epochMicros
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.PeerTransport
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle
import java.io.File

/** Only `adb shell run-as` creates this file in a debuggable build's private directory, so no other app can enable the agent. */
const val LIVE_LINK_AGENT_FLAG = "live-links-agent"
private const val LIVE_LINK_AGENT_PORT = 47110
private const val TAG = "RockyLiveLinks"

/**
 * Serves the live-link qualification agent on loopback, reached through `adb forward`, in a debuggable build with
 * [LIVE_LINK_AGENT_FLAG]. Its households live under `files/live-links`, apart from the app's own, and follow the app's
 * activities like the app's. See docs/features_08/06-qualification-evidence.md.
 */
fun startLiveLinkAgent(application: Application) {
    if (application.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0) return
    if (!File(application.filesDir, LIVE_LINK_AGENT_FLAG).exists()) return
    val lifecycle = RockyLifecycle.register(application)
    val agent = LiveLinkAgent(
        platform = { directory, name, transport ->
            ShoppingPlatform(
                directory = File(application.filesDir, "live-links/$directory").absolutePath,
                deviceName = name,
                room = { path -> Room.databaseBuilder<ShoppingDatabase>(application, path) },
                // One sealed-secret directory and Keystore key per household, so a later run never meets an earlier one's.
                secrets = { KeystoreSecretStore(File(application.noBackupFilesDir, "live-links-secrets-$directory"), "rocky.live-links.$directory") },
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
    Thread { agent.serve(LIVE_LINK_AGENT_PORT) }.start()
}
