package org.example.examples.shopping

import androidx.lifecycle.ViewModelStore
import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.session.SessionState
import org.example.examples.shopping.data.session.ShoppingNode
import org.example.examples.shopping.data.session.ShoppingPlatform
import org.example.examples.shopping.data.session.ShoppingSession
import org.example.examples.shopping.presentation.screen.lists.ListsViewModel
import rocky.PeerTransport
import java.io.File
import java.nio.file.Files
import kotlin.test.*

@OptIn(ExperimentalCoroutinesApi::class)
class LocalAvailabilityTest {
    private fun platform(root: File, name: String) = ShoppingPlatform(
        File(root, name).absolutePath,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
        transport = { PeerTransport.Iroh.isolated() },
    )

    @Test fun `checking an item commits and reaches its local observer while a full peer is offline`() = runBlocking<Unit> {
        Dispatchers.setMain(Dispatchers.Default)
        val root = Files.createTempDirectory("rocky-local-availability").toFile()
        val opened = mutableListOf<ShoppingNode>()
        var session: ShoppingSession? = null
        var observing: Job? = null
        val models = ViewModelStore()
        try {
            val desk = ShoppingNode.create(platform(root, "desk")).also { opened += it }
            var phone = ShoppingNode.join(platform(root, "phone"), desk.household.invite(NodeKind.Trusted, "Phone")).also { opened += it }
            val laptop = ShoppingNode.join(platform(root, "laptop"), desk.household.invite(NodeKind.Full, "Laptop")).also { opened += it }
            val list = desk.repository.createList("Weekly")
            desk.repository.add(list, "Milk")
            val item = withTimeout(30_000) { phone.repository.observe(list).first { it.isNotEmpty() }.first() }
            withTimeout(30_000) { laptop.repository.observe(list).first { it.isNotEmpty() } }
            phone.close()
            opened.remove(phone)
            val phoneSession = ShoppingSession(platform(root, "phone"))
            session = phoneSession
            phone = withTimeout(30_000) { (phoneSession.state.first { it is SessionState.Open } as SessionState.Open).node }
            val model = ListsViewModel(phoneSession)
            models.put("lists", model)
            observing = launch { model.state.collect() }
            model.select(list)
            try {
                withTimeout(30_000) { model.state.first { state -> state.items.any { it.id == item.id } } }
            } catch (error: TimeoutCancellationException) {
                println("LOCAL screen=${model.state.value} session=${phoneSession.state.value}")
                println("LOCAL lists=${withTimeoutOrNull(1_000) { phone.repository.lists().first() }}")
                println("LOCAL categories=${withTimeoutOrNull(1_000) { phone.repository.categories(list).first() }}")
                println("LOCAL items=${withTimeoutOrNull(1_000) { phone.repository.observe(list).first() }}")
                throw error
            }
            laptop.rocky.setOnline(false)
            repeat(10) { index ->
                val completed = index % 2 == 0
                val started = System.nanoTime()
                withTimeout(2_000) {
                    val observed = async(start = CoroutineStart.UNDISPATCHED) {
                        phone.repository.observe(list).first { rows -> rows.any { it.id == item.id && it.completed == completed } }
                    }
                    model.toggle(item.copy(completed = !completed))
                    assertEquals(completed, observed.await().single().completed)
                    assertEquals(completed, model.state.first { state -> state.items.any { it.id == item.id && it.completed == completed } }.items.single().completed)
                }
                println("LOCAL CHECK ${index + 1}: ${(System.nanoTime() - started) / 1_000_000} ms")
            }
        } finally {
            withContext(NonCancellable) {
                observing?.cancelAndJoin()
                models.clear()
                session?.close()
                opened.reversed().forEach { it.close() }
            }
            root.deleteRecursively()
            Dispatchers.resetMain()
        }
    }
}
