package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.collect
import java.io.File
import java.nio.file.Files
import kotlin.test.*

class RegistryObservationTest {
    @Test
    fun invitationObserversFollowLocalEnrollmentAndSyncedRegistryState() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-registry-observation").toFile()
        val definition = schema("registry_observation") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        fun configuration(name: String, online: Boolean = true) = Rocky.configure(File(root, name).path, definition) {
            networking {
                transport = PeerTransport.Iroh.isolated()
                enabled = online
                automaticSync = false
            }
        }
        val a = configuration("a").create()
        val opened = mutableListOf(a)
        val observers = mutableListOf<Job>()
        try {
            val b = configuration("b").join(a.credentials.pairing(GrantProfile.Issuer).uri).also { opened += it }
            val label = "Kitchen tablet".encodeToByteArray()
            val invitation = a.credentials.pairing(GrantProfile.Writer(setOf("items")), label)
            val local = Channel<InvitationState>(Channel.UNLIMITED)
            val remote = Channel<InvitationState>(Channel.UNLIMITED)
            val devices = Channel<List<Device>>(Channel.UNLIMITED)
            observers += launch { a.credentials.observeInvitation(invitation.uri).collect { local.send(it) } }
            observers += launch { b.credentials.observeInvitation(invitation.uri).collect { remote.send(it) } }
            observers += launch { b.credentials.observeDevices().collect { devices.send(it) } }
            assertTrue(withTimeout(10_000) { local.receive() }.open)
            assertNull(withTimeout(10_000) { remote.receive() }.enrolledNode)
            withTimeout(10_000) { devices.receive() }

            val c = configuration("c").join(invitation.uri).also { opened += it }
            val node = c.credentials.status().node
            suspend fun Channel<InvitationState>.enrollment(): InvitationState = withTimeout(10_000) {
                var state = receive()
                while (state.enrolledNode != node) state = receive()
                state
            }
            val enrolled = local.enrollment()
            assertFalse(enrolled.open)
            b.syncOnce()
            assertEquals(enrolled.id, remote.enrollment().id)
            val record = withTimeout(10_000) {
                var record = devices.receive().firstOrNull { it.node == node }
                while (record == null) record = devices.receive().firstOrNull { it.node == node }
                record
            }
            assertContentEquals(label, record.label)
            assertEquals(enrolled.id, record.invitationId)

            observers.forEach { it.cancelAndJoin() }
            // Replacing the invitation and reopening cannot erase its enrollment association.
            a.credentials.pairing(GrantProfile.Reader(setOf("items")))
            a.close()
            val reopened = configuration("a", online = false).open().also { opened += it }
            assertEquals(enrolled, reopened.credentials.invitation(invitation.uri))
        } finally {
            observers.forEach { it.cancelAndJoin() }
            opened.asReversed().forEach { it.close() }
            root.deleteRecursively()
        }
    }
}
