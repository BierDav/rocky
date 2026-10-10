package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.ConnectionState
import rocky.PeerTransport
import rocky.ReadinessState
import rocky.RockyDatabase
import rocky.RockyEvent
import rocky.room.buildOnRocky
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Device kinds: list keys reach full devices on their own, and partial devices only when shared. */
class HouseholdTest {
    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name),
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Tcp("127.0.0.1:0") },
    )
    private suspend fun ShoppingNode.titles(list: Long) = repository.observe(list).first().map { it.title }.sorted()

    /** Waits until [list] reads [titles] here; its rows may arrive before it is held. */
    private suspend fun ShoppingNode.awaitTitles(list: Long, vararg titles: String) = withTimeout(60_000) {
        repository.observe(list).first { rows -> rows.map { it.title }.sorted() == titles.toList() }
    }
    private suspend fun ShoppingNode.holds(list: Long) = rocky.list(list).status().isKeyed

    /** The household records an enrollment asynchronously, using its single pending invitation. */
    private suspend fun ShoppingNode.awaitEnrollment(node: ShoppingNode, name: String, kind: NodeKind) = withTimeout(30_000) {
        household.devices().first { devices -> devices.any { it.node == node.self && it.name == name && it.kind == kind.name.lowercase() } }
    }

    @Test fun `delayed enrollment events keep each invitation choice and leave the newer code open`() = runBlocking {
        val root = Files.createTempDirectory("rocky-invitation-order").toFile()
        val transport = { PeerTransport.Tcp("127.0.0.1:0") }
        val issuer = RockyDatabase.create(File(root, "issuer"), ShoppingSchema, transport(), automaticSync = false)
        val room = Room.databaseBuilder<ShoppingDatabase>(File(root, "room").path).buildOnRocky(issuer)
        val household = Household(room.room, issuer, issuer.session().node)
        val members = mutableListOf<RockyDatabase>()
        var recording: Job? = null
        try {
            val first = household.invite(NodeKind.Full, "Laptop")
            members += RockyDatabase.join(File(root, "full"), ShoppingSchema, first.invitation, transport(), automaticSync = false)
            val second = household.invite(NodeKind.Partial, "Phone")
            members += RockyDatabase.join(File(root, "partial"), ShoppingSchema, second.invitation, transport(), automaticSync = false)
            val unused = household.invite(NodeKind.Trusted, "Next")
            // All enrollments are retained before the application collector starts.
            recording = launch { household.run() }
            val devices = withTimeout(30_000) { household.devices().first { it.size == 2 } }
            assertEquals(setOf("Laptop" to "full", "Phone" to "partial"), devices.map { it.name to it.kind }.toSet())
            assertEquals(unused.toString(), household.invitations().first().single().code)
        } finally {
            withContext(NonCancellable) {
                recording?.cancelAndJoin()
                members.forEach { it.close() }
                room.close()
                issuer.close()
            }
            root.deleteRecursively()
        }
    }

    @Test fun `revocation keeps the new list keys away from the removed device and reaches remaining devices`() = runBlocking {
        val root = Files.createTempDirectory("rocky-revocation").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val trusted = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val removed = ShoppingNode.join(platform(root, "removed"), trusted.household.invite(NodeKind.Full, "Removed")).also { opened += it }
            trusted.awaitEnrollment(removed, "Removed", NodeKind.Full)
            val remaining = ShoppingNode.join(platform(root, "remaining"), trusted.household.invite(NodeKind.Full, "Remaining")).also { opened += it }
            val list = trusted.repository.createList("Weekly")
            trusted.repository.add(list, "Milk")
            removed.awaitTitles(list, "Milk")
            remaining.awaitTitles(list, "Milk")
            val epoch = trusted.rocky.list(list).status().epochs.max()
            removed.rocky.setOnline(false)
            trusted.household.revoke(removed.self)
            assertTrue(removed.self in trusted.household.revokedDevices().first())
            val rotated = trusted.rocky.list(list).status().epochs.max()
            assertTrue(rotated > epoch)
            withTimeout(60_000) {
                trusted.household.keys().first { keys -> keys.any { it.recipient == remaining.self && it.list == list && it.epoch == rotated.toString() } }
            }
            trusted.repository.add(list, "Bread")
            remaining.awaitTitles(list, "Bread", "Milk")
            assertTrue(trusted.household.keys().first().none { it.recipient == removed.self && it.list == list && it.epoch == rotated.toString() })
            assertEquals(listOf("Milk"), removed.titles(list))
            removed.rocky.setOnline(true)
            assertEquals(ConnectionState.Revoked, removed.rocky.syncOnce().state)
            assertEquals(ReadinessState.Revoked, removed.rocky.observeReadiness().first().state)
            removed.close()
            opened -= removed
            val session = ShoppingSession(platform(root, "removed"))
            try {
                val revoked = withTimeout(30_000) { session.state.first { it is SessionState.Revoked } } as SessionState.Revoked
                session.rejoin("not a pairing code")
                assertTrue(session.state.value is SessionState.Revoked)
                assertTrue(File(root, "removed/revoked.json").exists())
                session.rejoin(trusted.household.invite(NodeKind.Trusted, "Rejoined").toString())
                val state = withTimeout(60_000) { session.state.first { it is SessionState.Open || it is SessionState.Failed } }
                val rejoined = (state as? SessionState.Open)?.node ?: throw (state as SessionState.Failed).error
                assertNotEquals(revoked.node.self, rejoined.self)
                assertEquals("Rejoined", rejoined.household.devices().first().first { it.node == rejoined.self }.name)
                rejoined.awaitTitles(list, "Bread", "Milk")
                // This issuer also sees the old revoked row, which must not block delivery to live devices.
                val newList = rejoined.repository.createList("From rejoined device")
                rejoined.repository.add(newList, "Eggs")
                trusted.awaitTitles(newList, "Eggs")
                remaining.awaitTitles(newList, "Eggs")
                assertTrue(root.listFiles().orEmpty().any { it.name.startsWith("removed-revoked-") && File(it, "revoked.json").exists() })
            } finally {
                session.close()
            }
            trusted.close()
            opened -= trusted
            val reopened = ShoppingNode.resume(platform(root, "desk")).also { opened += it }
            assertTrue(removed.self in reopened.household.revokedDevices().first())
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }

    @Test fun `a failed reopen preserves the household and can retry after its lock is released`() = runBlocking {
        val root = Files.createTempDirectory("rocky-retry").toFile()
        val platform = platform(root, "desk")
        val original = ShoppingNode.create(platform)
        val list = original.repository.createList("Weekly")
        original.repository.add(list, "Milk")
        val session = ShoppingSession(platform)
        try {
            val failed = withTimeout(30_000) { session.state.first { it is SessionState.Failed } } as SessionState.Failed
            assertTrue(failed.existing)
            session.startOver()
            assertTrue(session.state.value is SessionState.Failed)
            original.close()
            session.retry()
            val reopened = withTimeout(30_000) { session.state.first { it is SessionState.Open } } as SessionState.Open
            assertEquals(listOf("Milk"), reopened.node.titles(list))
        } finally {
            withContext(NonCancellable) {
                session.close()
                original.close()
            }
            root.deleteRecursively()
        }
    }

    @Test fun `a full device receives every list and a partial one only shared lists`() = runBlocking {
        val root = Files.createTempDirectory("rocky-household").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val trusted = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val full = ShoppingNode.join(platform(root, "laptop"), trusted.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
            trusted.awaitEnrollment(full, "Laptop", NodeKind.Full)
            val partial = ShoppingNode.join(platform(root, "phone"), trusted.household.invite(NodeKind.Partial, "Phone")).also { opened += it }
            // The trusted device recorded both enrollments with the kinds it chose.
            withTimeout(30_000) {
                trusted.household.devices().first { rows -> rows.map { it.name to it.kind }.toSet() == setOf("desk" to "trusted", "Laptop" to "full", "Phone" to "partial") }
            }

            val weekly = trusted.repository.createList("Weekly")
            trusted.repository.add(weekly, "Milk")
            // No envelope is copied by hand: the full device installs the key sealed to it and catches up.
            full.awaitTitles(weekly, "Milk")
            // Joining installed its authors: it edits the list it received.
            full.repository.add(weekly, "Bread")
            trusted.awaitTitles(weekly, "Bread", "Milk")
            assertFalse(partial.holds(weekly))

            trusted.household.share(weekly, partial.self)
            partial.awaitTitles(weekly, "Bread", "Milk")
            val activity = partial.rocky.activity().first { it is RockyEvent.PartitionChanged && it.action == "key_installed" }
            assertEquals(listOf(weekly.toString()), (activity as RockyEvent.PartitionChanged).values)
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }

    @Test fun `lists two trusted devices create apart reach each other`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-trusted").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val phone = ShoppingNode.join(platform(root, "phone"), desk.household.invite(NodeKind.Trusted, "Phone")).also { opened += it }
            // Each holds a list the other has never seen when they next sync: neither direction may refuse it.
            for (node in opened) node.rocky.setOnline(false)
            val fromDesk = desk.repository.createList("From desk")
            desk.repository.add(fromDesk, "Milk")
            val fromPhone = phone.repository.createList("From phone")
            phone.repository.add(fromPhone, "Eggs")
            for (node in opened) node.rocky.setOnline(true)
            for (node in opened) {
                node.awaitTitles(fromDesk, "Milk")
                node.awaitTitles(fromPhone, "Eggs")
                withTimeout(30_000) { node.repository.lists().first { lists -> lists.map { it.title }.toSet() == setOf("From desk", "From phone") } }
            }
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }

    @Test fun `a received list keeps its items when the device reopens`() = runBlocking {
        val root = Files.createTempDirectory("rocky-household").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            val trusted = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            val code = trusted.household.invite(NodeKind.Full, "Laptop")
            var full = ShoppingNode.join(platform(root, "laptop"), code).also { opened += it }
            val weekly = trusted.repository.createList("Weekly")
            trusted.repository.add(weekly, "Milk")
            full.awaitTitles(weekly, "Milk")
            full.repository.add(weekly, "Eggs")
            trusted.awaitTitles(weekly, "Eggs", "Milk")
            full.close()
            opened -= full
            full = ShoppingNode.resume(platform(root, "laptop")).also { opened += it }
            assertEquals(listOf("Eggs", "Milk"), full.titles(weekly))
        } finally {
            withContext(NonCancellable) { opened.forEach { it.close() } }
            root.deleteRecursively()
        }
    }
}
