package org.example.examples.shopping

import androidx.room.Room
import java.io.File
import java.net.ServerSocket
import java.nio.file.Files
import kotlin.test.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.*

/** Approvals as strong commands, ordered by an in-process coordinator in place of the public service. */
class ApprovalsTest {
    private val coordinator = DevelopmentCoordinator()
    private fun platform(root: File, name: String) = ShoppingPlatform(File(root, name), name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Tcp(ServerSocket(0).use { "127.0.0.1:${it.localPort}" }) },
        commands = { CommandRuntime(coordinator, ApprovalHandlers) })

    @Test fun `a member asks, the first decision stands and a member cannot decide`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-approvals").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val phone = ShoppingNode.join(platform(root, "phone"), desk.household.invite(NodeKind.Full, "Phone")).also { opened += it }
            // The service registration of the app is replaced by registering with the in-process coordinator.
            coordinator.register(desk.rocky.commandIdentity(), callers = listOf(phone.rocky.commandIdentity()))

            assertNull(phone.approvals.ask("Coffee machine", "the old one leaks"))
            val asked = withTimeout(60_000) { desk.approvals.requests().first { it.isNotEmpty() } }.single()
            assertEquals("Coffee machine", asked.title)
            assertNull(asked.approved)
            // Identities map the authenticated command node to the recorded device.
            withTimeout(60_000) { desk.approvals.requests().first { it.single().requester == "Phone" } }

            assertNull(desk.approvals.decide(asked.key, approve = true))
            assertEquals("Another device already approved it", desk.approvals.decide(asked.key, approve = false))
            val decided = withTimeout(60_000) { phone.approvals.requests().first { it.singleOrNull()?.approved != null } }.single()
            assertEquals(true, decided.approved)
            assertEquals("desk", decided.decider)

            // Members hold no `Invoke` on `decisions`: the command is refused before it is stored.
            assertFails { phone.approvals.decide(asked.key, approve = false) }
            assertEquals("Another device already approved it", desk.approvals.decide(asked.key, approve = false))
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }
}
