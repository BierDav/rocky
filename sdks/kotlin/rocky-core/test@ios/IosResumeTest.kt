@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import kotlin.test.*

class IosResumeTest {
    @Test fun reclaimedStarterSocketsRebindAndAutomaticallySyncWithTheSameNode() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-resume-" + NSUUID().UUIDString
        val definition = schema("iosresume", 1) {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val network = PeerTransport.Iroh(relay = "disabled", discovery = false, bind = "127.0.0.1:0")
        val owner = RockyDatabase.create("$root/owner", definition, network)
        var member: RockyDatabase? = null
        try {
            owner.enterBackground()
            member = RockyDatabase.join("$root/member", definition, owner.pairing(GrantProfile.Writer(setOf("items"))).uri, network, automaticSync = false)
            owner.addPeer(member.contact().uri)
            val before = owner.contact().node
            member.enterBackground()
            owner.client.call(request("network_fault_close"))
            member.write(RockyDatabase.newHandle()) {
                val id = nextId("items")
                execute("INSERT INTO items(id,title) VALUES(?,'after suspension')", id)
            }
            owner.enterForeground()
            assertEquals(before, owner.contact().node)
            withTimeout(1_500) {
                owner.observe(query("SELECT title FROM items")).first { it.getValue("rows").jsonArray.isNotEmpty() }
            }
        } finally {
            member?.close()
            owner.close()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}
