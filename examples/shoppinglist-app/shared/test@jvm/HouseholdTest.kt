package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.flow.onStart
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.db.list
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import org.example.examples.shopping.data.session.ShoppingSession
import rocky.ConnectionState
import rocky.DeliveryState
import rocky.PeerTransport
import rocky.ReadinessState
import rocky.RockyDatabase
import rocky.RockyEvent
import rocky.query
import rocky.room.buildOnRocky
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** Device kinds: list keys reach full devices on their own, and partial devices only when shared. */
class HouseholdTest {
    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name).absolutePath,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Tcp("127.0.0.1:0") },
    )
    private suspend fun ShoppingNode.titles(list: Long) = repository.observe(list).first().map { it.title }.sorted()

    /** Waits until [list] reads [titles] here; its rows may arrive before it is held. */
    private suspend fun ShoppingNode.awaitTitles(list: Long, vararg titles: String, issuer: ShoppingNode? = null) = coroutineScope {
        val events = ArrayDeque<RockyEvent>()
        var observed = emptyList<String>()
        val recording = launch(start = CoroutineStart.UNDISPATCHED) {
            rocky.activity().collect {
                if (events.size == 128) events.removeFirst()
                events.addLast(it)
            }
        }
        try {
            withTimeout(60_000) {
                repository.observe(list).onEach { observed = it.map { row -> row.title }.sorted() }
                    .first { rows -> rows.map { it.title }.sorted() == titles.toList() }
            }
        } catch (timeout: TimeoutCancellationException) {
            withContext(NonCancellable) {
                runCatching {
                    withTimeout(2_000) {
                        println("list=$list expected=${titles.toList()} observed=$observed node=$self")
                        println("network=${rocky.network()}")
                        println("holding=${rocky.list(list).status()}")
                        println("activity=${events.toList()}")
                        val parameter = buildJsonObject {
                            put("type", "integer")
                            put("value", list)
                        }
                        println("rows=${runCatching { rocky.read(query("SELECT title FROM items WHERE list = ? ORDER BY title", JsonArray(listOf(parameter)))) }}")
                        if (issuer != null) {
                            println("issuer network=${issuer.rocky.network()}")
                            println("issuer devices=${issuer.rocky.devices().map { it.node to it.label?.decodeToString() }}")
                            println("issuer holding=${issuer.rocky.list(list).status()}")
                            println("issuer rows=${issuer.rocky.read(query("SELECT title FROM items WHERE list = ? ORDER BY title", JsonArray(listOf(parameter))))}")
                        }
                    }
                }.onFailure { println("delivery diagnostics failed: $it") }
            }
            throw timeout
        } finally {
            recording.cancelAndJoin()
        }
    }
    private suspend fun ShoppingNode.holds(list: Long) = rocky.list(list).status().isKeyed

    /** The household records an enrollment asynchronously, using its single pending invitation. */
    private suspend fun ShoppingNode.awaitEnrollment(node: ShoppingNode, name: String, kind: NodeKind) = withTimeout(30_000) {
        household.devices().first { devices -> devices.any { it.node == node.self && it.name == name && it.kind == kind.name.lowercase() } }
    }

    @Test fun `an undescribed member cannot receive every list from its grant profile`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-undescribed-member").toFile()
        val transport = { PeerTransport.Tcp("127.0.0.1:0") }
        val issuer = RockyDatabase.create(File(root, "issuer"), ShoppingSchema, transport(), automaticSync = false)
        val room = Room.databaseBuilder<ShoppingDatabase>(File(root, "room").path).buildOnRocky(issuer)
        var member: RockyDatabase? = null
        try {
            assertEquals(NodeKind.Full.profile, NodeKind.Partial.profile)
            member = RockyDatabase.join(File(root, "member"), ShoppingSchema, issuer.pairing(NodeKind.Partial.profile).uri, transport(), automaticSync = false)
            val node = member.session().node
            val household = Household(room.room, issuer, issuer.session().node)
            assertEquals("partial", household.devices().first().single { it.node == node }.kind)
            household.record(node, "Explicit full", NodeKind.Full)
            assertEquals("full", household.devices().first().single { it.node == node }.kind)
        } finally {
            member?.close()
            room.close()
            issuer.close()
            root.deleteRecursively()
        }
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
            val devices = withTimeout(30_000) { household.devices().first { rows -> rows.map { it.name }.toSet() == setOf("Laptop", "Phone") } }
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

    @Test fun `consumed invitations disappear after restarting without enrollment events`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-invitation-restart").toFile()
        val transport = { PeerTransport.Tcp("127.0.0.1:0") }
        val path = File(root, "issuer")
        var issuer = RockyDatabase.create(path, ShoppingSchema, transport(), automaticSync = false)
        var room = Room.databaseBuilder<ShoppingDatabase>(File(root, "room").path).buildOnRocky(issuer)
        var household = Household(room.room, issuer, issuer.session().node)
        var member: RockyDatabase? = null
        var nextMember: RockyDatabase? = null
        var recording: Job? = null
        try {
            val code = household.invite(NodeKind.Full, "Laptop")
            member = RockyDatabase.join(File(root, "member"), ShoppingSchema, code.invitation, transport(), automaticSync = false)
            assertEquals(code.toString(), household.invitations().first().single().code)
            room.close()
            issuer.close()
            issuer = RockyDatabase.resume(path, ShoppingSchema, transport(), automaticSync = false)
            room = Room.databaseBuilder<ShoppingDatabase>(File(root, "room").path).buildOnRocky(issuer)
            household = Household(room.room, issuer, issuer.session().node)
            recording = launch { household.run() }
            withTimeout(30_000) { household.invitations().first { it.isEmpty() } }
            val next = household.invite(NodeKind.Partial, "Next")
            assertTrue(issuer.invitationOpen(next.invitation))
            assertEquals(next.toString(), household.invitations().first().single().code)
            nextMember = RockyDatabase.join(File(root, "next"), ShoppingSchema, next.invitation, transport(), automaticSync = false)
            withTimeout(30_000) { household.invitations().first { it.isEmpty() } }
        } finally {
            withContext(NonCancellable) {
                recording?.cancelAndJoin()
                member?.close()
                nextMember?.close()
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
            // The rotation sealed the new epoch to the remaining device; nothing was sealed to the removed one.
            assertEquals(rotated, trusted.rocky.list(list).status().current)
            val sent = withTimeout(60_000) { trusted.household.sent().first { list in it[remaining.self].orEmpty() } }
            assertFalse(list in sent[removed.self].orEmpty())
            trusted.repository.add(list, "Bread")
            remaining.awaitTitles(list, "Bread", "Milk")
            // The remaining device acknowledged the rotated epoch at a sync.
            withTimeout(60_000) {
                trusted.rocky.activity().filterIsInstance<RockyEvent.EnvelopesDelivered>().onStart { emit(RockyEvent.EnvelopesDelivered(0, 0, "", 0, 0, 0, 0, 0)) }.first {
                    trusted.repository.deliveries(list).any { it.node == remaining.self && it.epoch == rotated && it.state == DeliveryState.Delivered }
                }
            }
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

    @Test fun `a reset closes the household and removes it, then setup can start a new one`() = runBlocking {
        val root = Files.createTempDirectory("rocky-reset").toFile()
        val closed = mutableListOf<RockyDatabase>()
        var erased = 0
        val platform = ShoppingPlatform(
            File(root, "desk").absolutePath,
            "desk",
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
            transport = { PeerTransport.Tcp("127.0.0.1:0") },
            closed = { closed += it },
            erase = { erased++ },
        )
        val session = ShoppingSession(platform)
        try {
            assertEquals(SessionState.Setup, session.state.value)
            session.create()
            val open = withTimeout(30_000) { session.state.first { it is SessionState.Open } } as SessionState.Open
            open.node.repository.createList("Weekly")
            val snapshot = CompletableDeferred<Unit>()
            val readiness = CompletableDeferred<Unit>()
            // Screen collectors may outlive reset's database close until navigation disposes them.
            val observers = listOf(
                launch { open.node.rocky.observe(rocky.query("SELECT id FROM lists")).collect { snapshot.complete(Unit) } },
                launch { open.node.rocky.observeReadiness().collect { readiness.complete(Unit) } },
                launch(start = CoroutineStart.UNDISPATCHED) { open.node.rocky.activity().collect() },
            )
            withTimeout(2_000) {
                snapshot.await()
                readiness.await()
            }
            session.reset()
            assertEquals(SessionState.Setup, withTimeout(30_000) { session.state.first { it is SessionState.Setup || it is SessionState.Failed } })
            withTimeout(2_000) { observers.joinAll() }
            assertTrue(observers.all { it.isCancelled })
            currentCoroutineContext().ensureActive()
            assertEquals(listOf(open.node.rocky), closed)
            assertEquals(1, erased)
            assertFalse(RockyDatabase.exists(platform.directory))
            session.create()
            val fresh = withTimeout(30_000) { session.state.first { it is SessionState.Open } } as SessionState.Open
            assertNotEquals(open.node.self, fresh.node.self)
            assertEquals(emptyList(), fresh.node.repository.lists().first())
        } finally {
            withContext(NonCancellable) { session.close() }
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
            // No envelope is copied by hand: sync delivers the key sealed to the full device, which catches up.
            full.awaitTitles(weekly, "Milk", issuer = trusted)
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
            full.awaitTitles(weekly, "Milk", issuer = trusted)
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
