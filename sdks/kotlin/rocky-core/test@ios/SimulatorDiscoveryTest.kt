@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.cinterop.toKString
import kotlinx.coroutines.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSString
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUTF8StringEncoding
import platform.Foundation.NSUUID
import platform.Foundation.writeToFile
import platform.posix.getenv
import kotlin.test.*

/** Local-network discovery on the simulator. With a host process it runs under qualify-ios-discovery. */
class SimulatorDiscoveryTest {
    private val definition = schema("iosdiscovery", 1) {
        table("items") {
            text("title")
            primaryKey("id")
        }
    }
    private fun local() = PeerTransport.Iroh(relay = "disabled", lookup = "disabled", topic = true, local = true, bind = "127.0.0.1:0")
    private fun write(path: String, text: String) {
        @Suppress("CAST_NEVER_SUCCEEDS")
        check((text as NSString).writeToFile(path, true, NSUTF8StringEncoding, null)) { "cannot write $path" }
    }

    @Test fun localDiscoveryRunsInTheForegroundAndStopsInTheBackground() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-discovery-" + NSUUID().UUIDString
        val owner = RockyDatabase.create("$root/owner", definition, local(), automaticSync = false)
        try {
            withTimeout(10_000) { while (!owner.discovery().announcing) delay(20) }
            assertEquals(DiscoveryPath(DiscoveryPathState.On), owner.discovery().local)
            // No lookup service is configured, and the status says so instead of staying silent.
            assertEquals(DiscoveryPathState.Unavailable, owner.discovery().topic.state)
            assertNotNull(owner.discovery().topic.reason)
            owner.enterBackground()
            assertEquals(DiscoveryPath(DiscoveryPathState.Off), owner.discovery().local)
            owner.enterForeground()
            assertEquals(DiscoveryPath(DiscoveryPathState.On), owner.discovery().local)
        } finally {
            owner.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }

    /** The simulator creates the domain; a host process joins it, and each finds the other over mDNS. */
    @Test fun simulatorAndHostProcessFindEachOtherOverMdns() = runBlocking<Unit> {
        val shared = getenv("ROCKY_DISCOVERY_ROOT")?.toKString() ?: return@runBlocking
        val owner = RockyDatabase.create("$shared/simulator", definition, local(), automaticSync = false)
        try {
            write("$shared/schema.json", definition.json())
            write("$shared/invitation", owner.pairing(GrantProfile.Writer(setOf("items"))).uri)
            val found = withTimeout(240_000) {
                var peer: Peer? = null
                while (peer == null) {
                    peer = owner.peers().firstOrNull { it.isLocal }
                    if (peer == null) delay(50)
                }
                peer
            }
            assertFalse(found.isIssuer)
            assertEquals(DiscoveryPath(DiscoveryPathState.On), owner.discovery().local)
            withTimeout(60_000) { while (!NSFileManager.defaultManager.fileExistsAtPath("$shared/host-found")) delay(50) }
            write("$shared/simulator-found", found.node)
        } finally {
            owner.close()
        }
    }
}
