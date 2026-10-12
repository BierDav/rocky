package rocky

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import java.io.File
import java.nio.file.Files
import kotlin.test.*

class DiscoveryTest {
    private val schema = schema("discovery") {
        table("items") {
            text("title")
            primaryKey("id")
        }
    }

    /** Local-network discovery only: no relay and no lookup service, so the topic path has nowhere to publish. */
    private fun local() = PeerTransport.Iroh(relay = "disabled", lookup = "disabled", topic = true, local = true, bind = "127.0.0.1:0")

    @Test
    fun pathsAreReportedAndTheLocalHoldFollowsTheLifecycle() = runBlocking {
        val root = Files.createTempDirectory("rocky-discovery").toFile()
        val a = RockyDatabase.create(File(root, "a"), schema, local(), automaticSync = false)
        try {
            withTimeout(10_000) { while (!a.discovery().announcing) delay(20) }
            val status = a.discovery()
            assertEquals(DiscoveryPath(DiscoveryPathState.On), status.local)
            assertEquals(DiscoveryPathState.Unavailable, status.topic.state)
            assertNotNull(status.topic.reason)
            assertEquals(status, a.network().discovery)

            // What Android's lifecycle does with the Wi-Fi multicast lock.
            var held = 0
            val hold = LocalDiscoveryHold({ held++ }, { held-- })
            hold.apply(a.discovery())
            hold.apply(a.discovery())
            assertEquals(1, held)
            a.enterBackground()
            assertEquals(DiscoveryPathState.Off, a.discovery().local.state)
            hold.apply(a.discovery())
            assertEquals(0, held)
            a.enterForeground()
            assertEquals(DiscoveryPathState.On, a.discovery().local.state)
            hold.apply(a.discovery())
            assertTrue(hold.isHeld)
            hold.apply(null)
            assertEquals(0, held)
        } finally {
            a.close()
            root.deleteRecursively()
        }
    }

    @Test
    fun aTransportWithoutDiscoveryReportsEachPathUnavailableWithItsReason() = runBlocking {
        val root = Files.createTempDirectory("rocky-discovery").toFile()
        val a = RockyDatabase.create(File(root, "a"), schema, PeerTransport.Iroh.isolated(), automaticSync = false)
        try {
            val status = a.discovery()
            assertFalse(status.announcing)
            for (path in listOf(status.local, status.topic)) {
                assertEquals(DiscoveryPath(DiscoveryPathState.Unavailable, "the iroh transport has no discovery"), path)
            }
        } finally {
            a.close()
            root.deleteRecursively()
        }
    }

    @Test
    fun aMemberFindsTheIssuerOnTheLocalNetwork() = runBlocking {
        val root = Files.createTempDirectory("rocky-discovery").toFile()
        val a = RockyDatabase.create(File(root, "a"), schema, local(), automaticSync = false)
        var b: RockyDatabase? = null
        try {
            val invitation = a.pairing(GrantProfile.Writer(setOf("items")))
            val member = RockyDatabase.join(File(root, "b"), schema, invitation.uri, local(), automaticSync = false).also { b = it }
            member.syncOnce()
            val found = withTimeout(30_000) {
                var peer: Peer? = null
                while (peer == null) {
                    peer = member.peers().firstOrNull { it.isLocal }
                    if (peer == null) delay(50)
                }
                peer
            }
            assertTrue(found.isIssuer)
            assertTrue(member.discovery().announcing)
        } finally {
            b?.close()
            a.close()
            root.deleteRecursively()
        }
    }
}
