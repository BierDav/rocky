package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.data.coordination.ApprovalHandlers
import org.example.examples.shopping.data.coordination.Approvals
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.household.commandIdentity
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import rocky.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Approvals as strong commands, ordered by an in-process coordinator in place of the public service. */
class ApprovalsTest {
    private val coordinator = DevelopmentCoordinator()
    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name).absolutePath,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Iroh.isolated() },
        commands = { CommandRuntime(coordinator, ApprovalHandlers) },
    )

    @Test fun `registry callers work on an executor that never wrote or synced a roster row`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-rosterless-approvals").toFile()
        val nodes = mutableListOf<RockyDatabase>()
        val rooms = mutableListOf<ShoppingDatabase>()
        val coordinator = DevelopmentCoordinator()
        fun transport() = PeerTransport.Iroh.isolated()
        fun commands() = CommandRuntime(coordinator, ApprovalHandlers)
        try {
            val executor = RockyDatabase.create(File(root, "executor"), ShoppingSchema, transport(), commands = commands(), automaticSync = false).also(nodes::add)
            val member = RockyDatabase.join(File(root, "member"), ShoppingSchema, executor.pairing(NodeKind.Full.profile).uri, transport(), commands = commands(), automaticSync = false).also(nodes::add)
            val executorRoom = openShoppingDatabase(File(root, "executor"), executor).also(rooms::add)
            val memberRoom = openShoppingDatabase(File(root, "member"), member).also(rooms::add)
            executor.setDeviceLabel(member.session().node, "{\"name\":\"Phone\",\"kind\":\"full\"}".encodeToByteArray())
            coordinator.register(executor.commandIdentity(), callers = executor.devices().map { it.commandIdentity(executor.commandIdentity()) })
            val trusted = Approvals(executorRoom, executor, Household(executorRoom, executor, executor.session().node), executor.session().node, true)
            val applicant = Approvals(memberRoom, member, Household(memberRoom, member, member.session().node), member.session().node, true)
            assertTrue(Row.decode(executor.read(ShoppingSchema.table("devices").query())).isEmpty())
            assertNull(applicant.ask("Coffee machine", "Replacement"))
            val request = withTimeout(60_000) { trusted.requests().first { it.isNotEmpty() } }.single()
            assertEquals("Phone", request.requester)
            assertNull(trusted.decide(request.key, approve = true))
            val decision = withTimeout(60_000) {
                var rows = applicant.requests().first()
                while (rows.singleOrNull()?.approved == null) {
                    member.syncOnce()
                    rows = applicant.requests().first()
                }
                rows.single()
            }
            assertEquals(true, decision.approved)
            assertEquals("This device", decision.requester)
            assertEquals("Another device", decision.decider)
            assertTrue(Row.decode(executor.read(ShoppingSchema.table("devices").query())).isEmpty())
            assertEquals(ConnectionState.Connected, executor.syncOnce().state)
            val foreignHex = executor.session().node
            val foreignKey = foreignHex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
            memberRoom.close()
            rooms.remove(memberRoom)
            member.close()
            nodes.remove(member)
            File(root, "member").walkTopDown().filter { it.isFile }.forEach { file ->
                val bytes = file.readBytes()
                for (needle in listOf(foreignKey, foreignHex.encodeToByteArray())) {
                    assertFalse(
                        bytes.indices.any { start ->
                            start + needle.size <= bytes.size && needle.indices.all { bytes[start + it] == needle[it] }
                        },
                        "Foreign executor node key in ${file.name}",
                    )
                }
            }
        } finally {
            rooms.asReversed().forEach { it.close() }
            nodes.asReversed().forEach { it.close() }
            root.deleteRecursively()
        }
    }

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
            assertEquals("Another device", decided.decider)
            assertEquals("This device", decided.requester)

            // Members hold no `Invoke` on `decisions`: the command is refused before it is stored.
            assertFails { phone.approvals.decide(asked.key, approve = false) }
            assertEquals("Another device already approved it", desk.approvals.decide(asked.key, approve = false))
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }
}
