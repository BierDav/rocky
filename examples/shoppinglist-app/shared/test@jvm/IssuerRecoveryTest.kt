package org.example.examples.shopping

import kotlinx.coroutines.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.example.examples.shopping.data.ShoppingRepository
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.household.NodeKind
import rocky.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** The starter pairing makes a second issuer, which carries the list once the creator device is lost. */
class IssuerRecoveryTest {
    private fun loopback(): PeerTransport = PeerTransport.Iroh.isolated()

    /** Room checks every table it declares, so a device reads the household and approval tables too. */
    private val writer = NodeKind.Full.profile
    private val rooms = mutableMapOf<RockyDatabase, ShoppingDatabase>()
    private var list = 0L
    private fun RockyDatabase.withRoom(directory: File) = also { rooms[it] = openShoppingDatabase(directory, it) }
    private val RockyDatabase.repository get() = ShoppingRepository(rooms.getValue(this), this)
    private suspend fun RockyDatabase.add(title: String) = repository.add(list, title)
    private suspend fun RockyDatabase.titles() = rooms.getValue(this).items().all(list).map { it.title }.sorted()

    /** The issuer shares the list's current secret with [readers]; each one's next sync delivers it. */
    private suspend fun RockyDatabase.share(vararg readers: RockyDatabase) {
        repository.share(list, readers.map { it.session().node })
    }

    /** A joined device finishes its own bootstrap before it starts a list from a checkpoint. */
    private suspend fun RockyDatabase.joined() = also { assertTrue(syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } }) }
    private suspend fun close(runtime: RockyDatabase) {
        rooms.remove(runtime)?.close()
        runtime.close()
    }
    private suspend fun join(directory: File, invitation: PairingInvitation) = RockyDatabase.join(directory, ShoppingSchema, invitation.uri, loopback(), automaticSync = false)

    @Test fun `a second device invites and renews after the creator device is destroyed`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-issuers").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, loopback(), automaticSync = false).withRoom(File(root, "a")).also { opened += it }
            a.tips.createBookmark("main")
            a.tips.follow("main")
            list = a.repository.createList("Groceries")
            val b = join(File(root, "b"), a.pairDevice()).withRoom(File(root, "b")).also { opened += it }.joined()
            assertTrue(b.isIssuer)
            assertEquals(GrantProfile.Issuer, b.session().profile)
            assertNotEquals(a.session().node, b.session().node)
            assertNotEquals(a.session().principal, b.session().principal)
            // A credential the creator issued for one minute, to a device that knows both issuers.
            val keys = Credentials.prepare(File(root, "d"))
            val supplied = a.grant(keys, writer, ttlSeconds = 60)
            val d = RockyDatabase.install(File(root, "d"), ShoppingSchema, supplied, transport = loopback()).withRoom(File(root, "d")).also { opened += it }
            val expiring = d.session()
            a.share(b, d)
            a.add("Milk")
            assertTrue(d.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            // A credential installed without enrollment follows nothing until the application says so.
            d.tips.follow("main")
            assertTrue(b.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            assertEquals(listOf("Milk"), b.titles())

            // The creator device is destroyed, keys included; the second issuer revokes it.
            val creator = a.session().node
            close(a)
            opened -= a
            assertTrue(File(root, "a").deleteRecursively())
            b.revoke(creator)
            assertEquals(expiring.revocations.statements + 1uL, b.session().revocations.statements)
            assertTrue(b.peers().none { it.node == creator })

            // A device joining after the rotation starts the list from a base at a seal.
            b.tips.seal()
            b.tips.compact()
            val c = join(File(root, "c"), b.pairing(writer)).withRoom(File(root, "c")).also { opened += it }.joined()
            // Revocation rotated the list's secret; the second issuer shares the new epoch with the device
            // enrolled since, which starts the list from its checkpoint, as it rejects the revoked creator's frames.
            b.share(c)
            assertTrue(c.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            assertEquals(listOf("Milk"), c.titles())

            val renewed = d.renew()
            assertEquals(b.session().revocations, renewed.revocations)
            assertTrue(renewed.expiresAtEpochSeconds > expiring.expiresAtEpochSeconds + 24 * 60 * 60)
            assertTrue(d.peers().none { it.node == creator })
            // The rotation sealed the new epoch to the recorded reader itself: the held list gains it at the
            // next sync, and renewal's bootstrap then restarts it from a checkpoint.
            assertTrue(d.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            // Its authors ended with the revoked epoch: the second issuer assigns new ones over the network.
            d.add("Eggs")
            assertTrue(d.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            assertTrue(c.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            assertEquals(listOf("Eggs", "Milk"), c.titles())
        } finally {
            opened.forEach { close(it) }
            root.deleteRecursively()
        }
    }

    @Test fun `a full member bootstraps from a second trusted device with both peers online`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-two-trusted-bootstrap").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, loopback(), automaticSync = false).withRoom(File(root, "a")).also(opened::add)
            a.tips.createBookmark("main")
            a.tips.follow("main")
            a.tips.setGenesisMinimum(1u)
            list = a.repository.createList("Groceries")
            a.add("Milk")
            val b = join(File(root, "b"), a.pairDevice()).withRoom(File(root, "b")).also(opened::add).joined()
            a.share(b)
            b.joined()
            assertTrue(a.isIssuer && b.isIssuer)
            assertEquals(a.titles(), b.titles())
            val member = join(File(root, "member"), b.pairing(writer)).withRoom(File(root, "member")).also(opened::add)
            member.partitions.role(NodeKind.Full.role)
            b.share(member)
            member.addPeer(a.contact().uri)
            member.joined()
            val status = member.status()
            assertFalse(status.data.bootstrapPending)
            assertTrue(status.data.revision > 0uL)
            assertEquals(listOf("Milk"), member.titles())
            assertFalse(member.isIssuer)
            assertEquals(writer, member.session().profile)
            // Edits remain authorized and converge after enrollment; no role escalation or re-pairing.
            member.add("Eggs")
            member.joined()
            a.joined()
            b.joined()
            assertEquals(listOf("Eggs", "Milk"), a.titles())
            assertEquals(a.titles(), b.titles())
            assertEquals(a.titles(), member.titles())
        } finally {
            opened.asReversed().forEach { close(it) }
            root.deleteRecursively()
        }
    }

    @Test fun `a scoped grant adds list creation to an enrolled member without changing its identity`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-member-grant").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, loopback(), automaticSync = false).withRoom(File(root, "a")).also(opened::add)
            val member = join(File(root, "member"), a.pairing(NodeKind.Partial.profile)).withRoom(File(root, "member")).also(opened::add).joined()
            val before = member.session()
            assertFailsWith<RockyException> { member.repository.createList("Before grant") }
            val supplied = a.grant(
                buildJsonObject {
                    put("user", before.principal)
                    put("node", before.node)
                },
                NodeKind.Full.profile,
            )
            member.install(supplied)
            val after = member.session()
            assertEquals(before.node, after.node)
            assertEquals(before.principal, after.principal)
            assertFalse(after.isIssuer)
            list = member.repository.createList("After grant")
            member.add("Milk")
            member.share(a)
            member.joined()
            assertEquals(listOf("Milk"), a.titles())
            assertEquals(a.titles(), member.titles())
        } finally {
            opened.asReversed().forEach { close(it) }
            root.deleteRecursively()
        }
    }

    @Test fun `a device paired with the opt-out cannot issue`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-issuers").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, loopback(), automaticSync = false).withRoom(File(root, "a")).also { opened += it }
            list = a.repository.createList("Groceries")
            val x = join(File(root, "x"), a.pairDevice(issuing = false)).withRoom(File(root, "x")).also { opened += it }.joined()
            assertFalse(x.isIssuer)
            // Joining asked the creator for its authors; a non-issuer cannot assign its own.
            // The starter opt-out writes every table, the trusted-only ones included.
            assertEquals(GrantProfile.Writer(ShoppingSchema.tables.map { it.name }.toSet()), x.session().profile)
            val keys = Credentials.prepare(File(root, "y"))
            assertFailsWith<RockyException> { x.pairDevice() }
            assertFailsWith<RockyException> { x.pairing(writer) }
            assertFailsWith<RockyException> { x.grant(keys, writer) }
            assertFailsWith<RockyException> { x.delegate(keys, writer) }
            assertFailsWith<RockyException> { x.revoke(a.session().node) }
            a.share(x)
            // The list arrives before an item can reference it.
            assertTrue(x.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            x.repository.share(list, listOf(a.session().node))
            x.add("Milk")
            assertTrue(x.syncOnce().peers.any { peer -> peer.attempts.any { attempt -> attempt.operation == rocky.AttemptOperation.Reconcile && attempt.outcome == rocky.AttemptOutcome.Completed } })
            assertEquals(listOf("Milk"), a.titles())
            // The creator still holds the only publisher key and keeps its logout guard.
            assertFailsWith<RockyException> { a.logout() }
        } finally {
            opened.forEach { close(it) }
            root.deleteRecursively()
        }
    }
}
