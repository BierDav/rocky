@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import kotlin.test.*
import kotlin.time.TimeSource

class IosSyncLifecycleTest {
    @Test
    fun backgroundCancelsNativeDialAndForegroundStartsImmediately() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-sync-lifecycle-" + NSUUID().UUIDString
        val schema = schema("lifecycle") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val a = RockyDatabase.create("$root/a", schema, PeerTransport.Iroh.isolated())
        try {
            val b = RockyDatabase.join("$root/b", schema, a.pairing(GrantProfile.Issuer).uri, PeerTransport.Iroh.isolated(), automaticSync = false)
            a.addPeer(b.contact().uri)
            // Nobody answers the closed peer's route, so a's dial stays open until cancelled.
            b.close()
            fun dialing(readiness: DatabaseStatus) = readiness.network.peers.any { it.link.state == LinkState.Opening }
            a.setNetworkingEnabled(true)
            withTimeout(5_000) { a.observeStatus().first(::dialing) }
            val before = TimeSource.Monotonic.markNow()
            withTimeout(1_000) { a.enterBackground() }
            val background = before.elapsedNow()
            assertTrue(background.inWholeMilliseconds < 1_000)
            assertTrue(a.network().peers.none { it.link.state == LinkState.Opening })
            val foreground = TimeSource.Monotonic.markNow()
            // Foregrounding resumes the enabled worker without an explicit sync.
            withTimeout(1_000) {
                a.enterForeground()
                a.observeStatus().first(::dialing)
            }
            println("background cancelled in ${background.inWholeMilliseconds} ms; foreground dial in ${foreground.elapsedNow().inWholeMilliseconds} ms")
            withTimeout(1_000) { a.enterBackground() }
        } finally {
            a.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
