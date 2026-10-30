package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import org.example.examples.shopping.data.tips.Follow
import org.example.examples.shopping.data.tips.HeldItem
import org.example.examples.shopping.data.tips.HouseholdTips
import org.example.examples.shopping.data.tips.MAIN
import rocky.KeptSeals
import rocky.PeerTransport
import rocky.TipHeld
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** The app's seal policy, kept seals, branches and offline reopening; see `docs/runtime/shopping-seals.md`. */
class SealsTest {
    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name).absolutePath,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Iroh.isolated() },
    )

    private suspend fun ShoppingNode.titles(list: Long) = repository.observe(list).first().map { it.title }.sorted()

    /** Polls [condition] with a sync pass in between, so a slow link cannot hide a missing delivery for long. */
    private suspend fun await(what: String, vararg nodes: ShoppingNode, condition: suspend () -> Boolean) {
        try {
            withTimeout(60_000) {
                while (!condition()) {
                    nodes.forEach { runCatching { it.rocky.syncOnce() } }
                    delay(200)
                }
            }
        } catch (timeout: TimeoutCancellationException) {
            throw AssertionError("timed out waiting for $what", timeout)
        }
    }

    private fun household(test: suspend CoroutineScope.(root: File, opened: MutableList<ShoppingNode>) -> Unit) = runBlocking {
        val root = Files.createTempDirectory("rocky-shopping-seals").toFile()
        val opened = mutableListOf<ShoppingNode>()
        try {
            test(root, opened)
        } finally {
            withContext(NonCancellable) { opened.asReversed().forEach { runCatching { it.close() } } }
            root.deleteRecursively()
        }
    }

    @Test fun `a trusted device seals by policy and shows a kept seal, and a member compacts`() = household { root, opened ->
        val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
        val laptop = ShoppingNode.join(platform(root, "laptop"), desk.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
        val list = desk.repository.createList("Weekly")
        desk.repository.add(list, "Milk")
        val policy = HouseholdTips(desk.rocky, platform(root, "desk").room, File(root, "desk").path, sealAfter = 3u)
        val first = desk.tips.seal().tip
        assertNull(policy.maintain().sealed, "sealed below the policy's threshold")
        assertEquals(KeptSeals(every = true), desk.rocky.tips.kept())
        desk.repository.add(list, "Eggs")
        desk.repository.add(list, "Bread")
        desk.repository.add(list, "Jam")
        val second = assertNotNull(policy.maintain().sealed, "the policy did not seal after three writes")
        desk.repository.add(list, "Butter")

        // The view of the first seal reads the same DAOs on the state it sealed.
        val held = desk.tips.refresh().path.associate { it.id to it.held }
        assertEquals(TipHeld.KEPT, held[first])
        val sealed = desk.tips.snapshot(first).single()
        assertEquals("Weekly" to listOf(HeldItem("Milk", false)), sealed.title to sealed.items)
        assertEquals(listOf("Bread", "Eggs", "Jam", "Milk"), desk.tips.snapshot(second).single().items.map { it.title }.sorted())

        await("the laptop to follow both seals", laptop) { laptop.rocky.tips.status().tip == desk.rocky.tips.status().tip && laptop.titles(list).size == 5 }
        // Compaction also needs every frame through the seal's frontier, which the titles do not show.
        await("the laptop to compact the first seal", laptop) { laptop.tips.maintain().compaction.compacted == first }
        assertEquals(KeptSeals(), laptop.rocky.tips.kept())
        assertEquals(listOf("Bread", "Butter", "Eggs", "Jam", "Milk"), laptop.titles(list))
    }

    @Test fun `a branch carries only its followers' writes and a member drops it on return`() = household { root, opened ->
        val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
        val laptop = ShoppingNode.join(platform(root, "laptop"), desk.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
        val phone = ShoppingNode.join(platform(root, "phone"), desk.household.invite(NodeKind.Full, "Phone")).also { opened += it }
        val list = desk.repository.createList("Weekly")
        desk.repository.add(list, "Milk")
        await("both members to hold the list", laptop, phone) { laptop.titles(list) == listOf("Milk") && phone.titles(list) == listOf("Milk") }

        val dev = desk.tips.branch("dev")
        assertEquals("dev", desk.rocky.tips.status().bookmark)
        desk.repository.add(list, "Desk on dev")
        await("the laptop to learn dev", laptop) { laptop.tips.refresh().bookmarks.any { it.name == "dev" } }
        if (laptop.tips.follow("dev") == Follow.Pulling) {
            await("the laptop to switch to dev", laptop, desk) {
                laptop.tips.maintain()
                laptop.rocky.tips.status().bookmark == "dev"
            }
        }
        await("the laptop to receive dev's write", laptop, desk) { "Desk on dev" in laptop.titles(list) }
        laptop.repository.add(list, "Laptop on dev")
        await("the desk to receive the laptop's dev write", desk, laptop) { "Laptop on dev" in desk.titles(list) }
        phone.repository.add(list, "Phone on main")
        repeat(3) { phone.rocky.syncOnce() }
        assertEquals(listOf("Milk", "Phone on main"), phone.titles(list), "the phone received a dev write")
        assertFalse(phone.rocky.tips.inspect().nodes.any { node -> node.keptTips?.contains(dev) == true }, "the phone holds dev frames")

        laptop.tips.leave("dev")
        assertEquals(MAIN, laptop.rocky.tips.status().bookmark)
        await("the laptop to receive main's write", laptop, phone) { laptop.titles(list) == listOf("Milk", "Phone on main") }
        desk.tips.leave("dev", delete = true)
        await("the desk to return to main", desk, phone) { desk.titles(list) == listOf("Milk", "Phone on main") }
        await("dev's deletion to reach the laptop", laptop, desk) { laptop.tips.refresh().bookmarks.map { it.name } == listOf(MAIN) }
    }

    @Test fun `a full member keeps observing main while two trusted nodes seal and one opens a branch`() = household { root, opened ->
        val a = ShoppingNode.create(platform(root, "a")).also(opened::add)
        val b = ShoppingNode.join(platform(root, "b"), a.household.invite(NodeKind.Trusted, "B")).also(opened::add)
        val full = ShoppingNode.join(platform(root, "full"), b.household.invite(NodeKind.Full, "Full")).also(opened::add)
        val list = a.repository.createList("Weekly")
        a.repository.add(list, "Before")
        await("the trusted peer and full member to bootstrap", b, full) { b.titles(list) == listOf("Before") && full.titles(list) == listOf("Before") }
        val observed = mutableListOf<List<String>>()
        val observer = launch { full.repository.observe(list).collect { rows -> observed += rows.map { it.title }.sorted() } }
        try {
            full.setOnline(false)
            a.tips.seal()
            a.repository.add(list, "On main")
            a.tips.seal()
            await("B to hold main's seals", a, b) { b.titles(list) == listOf("Before", "On main") }
            a.tips.branch("dev")
            a.repository.add(list, "Only dev")
            full.setOnline(true)
            await("the member to recover main through B", b, full) {
                full.titles(list) == listOf("Before", "On main") && !full.rocky.status().data.bootstrapPending
            }
            assertEquals(MAIN, full.rocky.tips.status().bookmark)
            assertEquals(MAIN, b.rocky.tips.status().bookmark)
            assertEquals("dev", a.rocky.tips.status().bookmark)
            assertFalse(full.failure.isCompleted)
            assertTrue(observer.isActive)
            await("the live observer to publish main's rows", b, full) { observed.lastOrNull() == listOf("Before", "On main") }
            val created = full.repository.createList("From full on main")
            full.repository.add(created, "New on main")
            await("the member's new list to reach the trusted main follower", b, full) {
                b.titles(created) == listOf("New on main")
            }
            val devLists = withTimeout(60_000) { a.repository.lists().first() }
            assertTrue(devLists.none { it.id == created }, "the dev follower received main's new list")
            a.tips.follow(MAIN)
            await("the new list to bootstrap after returning to main", a, b, full) {
                a.titles(created) == listOf("New on main")
            }
        } finally {
            observer.cancelAndJoin()
        }
    }

    @Test fun `a device switched offline reopens without a network and publishes on switching back`() = household { root, opened ->
        val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
        val laptop = ShoppingNode.join(platform(root, "laptop"), desk.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
        val list = desk.repository.createList("Weekly")
        await("the laptop to hold the list", laptop) { laptop.repository.lists().first().isNotEmpty() }
        desk.setOnline(false)
        desk.close()
        opened.remove(desk)
        val offline = ShoppingNode.resume(platform(root, "desk")).also { opened += it }
        assertFalse(offline.online.value)
        assertNull(offline.rocky.openedNetwork)
        offline.repository.add(list, "Written offline")
        repeat(3) { laptop.rocky.syncOnce() }
        assertTrue(laptop.titles(list).isEmpty(), "an offline write left the device")
        offline.setOnline(true)
        assertTrue(offline.online.value)
        await("the offline write to arrive", laptop, offline) { laptop.titles(list) == listOf("Written offline") }
    }
}
