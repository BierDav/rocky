package rocky

import kotlinx.coroutines.*
import java.io.File
import java.nio.file.Files
import java.util.concurrent.ConcurrentLinkedQueue
import kotlin.test.*

class DiagnosticsTest {
    @Test
    fun aSyncReachesTheListenerAsSpansWithoutTheInvitation() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-diagnostics").toFile()
        val definition = schema("diagnostics") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val events = ConcurrentLinkedQueue<DiagnosticEvent>()
        RockyDiagnostics.listen(DiagnosticLevel.DEBUG) { events.add(it) }
        val a = RockyDatabase.create(File(root, "a"), definition, PeerTransport.Iroh.isolated())
        var b: RockyDatabase? = null
        try {
            val invitation = a.pairing(GrantProfile.Issuer).uri
            b = RockyDatabase.join(File(root, "b"), definition, invitation, PeerTransport.Iroh.isolated(), automaticSync = false)
            b.syncOnce()
            assertTrue(b.status().data.blockers.none { it.kind is rocky.internal.native.BlockerKind.PendingFrames })
            withTimeout(5_000) {
                while (events.none { it.name == "sync_pass" && it.durationMillis != null }) delay(10)
            }
            val admissions = events.filter { it.name == "admission" }
            assertTrue(admissions.any { "outcome=ok" in it.fields }, admissions.joinToString("\n") { it.render() })
            assertTrue(events.none { invitation in it.render() })
        } finally {
            RockyDiagnostics.listen(DiagnosticLevel.ERROR, null)
            b?.close()
            a.close()
            root.deleteRecursively()
        }
    }
}
