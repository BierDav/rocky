package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import java.io.File
import java.nio.file.Files
import kotlin.test.*

class SyncLifecycleTest {
    @Test
    fun backgroundClosesLinksAndForegroundReopensThem() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-sync-lifecycle").toFile()
        val definition = schema("lifecycle") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val a = RockyDatabase.create(File(root, "a"), definition, PeerTransport.Iroh.isolated())
        var b: RockyDatabase? = null
        try {
            b = RockyDatabase.join(File(root, "b"), definition, a.pairing(GrantProfile.Issuer).uri, PeerTransport.Iroh.isolated())
            a.addPeer(b.contact().uri)
            val opened = withTimeout(5_000) { a.observeStatus().first { it.network.peers.any { peer -> peer.link.state == LinkState.Open } } }
            val link = opened.network.peers.first { it.link.state == LinkState.Open }.link
            assertNotNull(link.openedAtEpochSeconds)
            // The loopback connection reports its one direct path.
            val path = a.network().peers.first { it.link.state == LinkState.Open }.link.paths.first()
            assertTrue(path.selected)
            assertEquals("direct", path.kind)
            assertTrue(path.remote.startsWith("127.0.0.1:"))
            withTimeout(1_000) { a.enterBackground() }
            assertTrue(a.network().peers.all { it.link.state == LinkState.Closed && it.link.paths.isEmpty() })
            val closed = withTimeout(1_000) { a.activity().first { it is RockyEvent.LinkClosed && it.reason == "suspended" } } as RockyEvent.LinkClosed
            assertNull(closed.error)
            a.enterForeground()
            withTimeout(5_000) { a.observeStatus().first { it.network.peers.any { peer -> peer.link.state == LinkState.Open } } }
        } finally {
            a.enterBackground()
            b?.enterBackground()
            b?.close()
            a.close()
            root.deleteRecursively()
        }
    }

    @Test
    fun backgroundCancelsNativeDialAndForegroundStartsImmediately() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-sync-lifecycle").toFile()
        val schema = schema("lifecycle") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val a = RockyDatabase.create(File(root, "a"), schema, PeerTransport.Iroh.isolated())
        try {
            val b = RockyDatabase.join(File(root, "b"), schema, a.pairing(GrantProfile.Issuer).uri, PeerTransport.Iroh.isolated(), automaticSync = false)
            a.addPeer(b.contact().uri)
            // Nobody answers the closed peer's route, so a's dial stays open until cancelled.
            b.close()
            fun dialing(readiness: DatabaseStatus) = readiness.network.peers.any { it.link.state == LinkState.Opening }
            a.setNetworkingEnabled(true)
            withTimeout(5_000) { a.observeStatus().first(::dialing) }
            val before = System.nanoTime()
            withTimeout(1_000) { a.enterBackground() }
            val background = System.nanoTime() - before
            assertTrue(background < 1_000_000_000)
            assertTrue(a.network().peers.none { it.link.state == LinkState.Opening })
            val foreground = System.nanoTime()
            // Foregrounding resumes the enabled worker without an explicit sync.
            withTimeout(1_000) {
                a.enterForeground()
                a.observeStatus().first(::dialing)
            }
            println("background cancelled in ${background / 1_000_000} ms; foreground dial in ${(System.nanoTime() - foreground) / 1_000_000} ms")
            withTimeout(1_000) { a.enterBackground() }
        } finally {
            a.close()
            root.deleteRecursively()
        }
    }
}
