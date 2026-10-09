package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.RockyDatabase
import java.io.File

/** A developer walkthrough of a shared list; the Compose app uses the same [ShoppingNode]. */
fun main() = runBlocking {
    val directory = File(requireNotNull(System.getenv("ROCKY_DEMO_DATA")))
    val creatorDirectory = File(directory, "creator")
    fun platform(directory: File, name: String) = ShoppingPlatform(
        directory,
        name,
        room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) },
    )
    val creatorPlatform = platform(creatorDirectory, "Desktop")
    val creator = if (RockyDatabase.exists(creatorDirectory)) {
        ShoppingNode.resume(creatorPlatform)
    } else {
        ShoppingNode.create(creatorPlatform)
    }
    var peer: ShoppingNode? = null
    try {
        val peerDirectory = File(directory, "peer")
        val peerPlatform = platform(peerDirectory, "Peer")
        peer = if (RockyDatabase.exists(peerDirectory)) {
            ShoppingNode.resume(peerPlatform)
        } else {
            val code = creator.household.invite(NodeKind.Full, "Peer")
            println("Transport: ${creator.rocky.session().transport}; QR payload: $code")
            ShoppingNode.join(peerPlatform, code)
        }
        val list = creator.repository.createList("Groceries")
        // One envelope shares the list with its categories and items.
        val node = peer.self
        peer.repository.accept(list, creator.repository.share(list, listOf(node)).getValue(node))
        val dairy = creator.repository.addCategory(list, "Dairy")
        creator.repository.add(list, "Milk", category = dairy)
        val rows = withTimeout(10_000) {
            peer.repository.observe(list).first { items -> items.any { it.title == "Milk" && !it.completed } }
        }
        println("Live Room query on peer: $rows")
        val milk = rows.first { it.title == "Milk" && !it.completed }
        peer.rocky.setOnline(false)
        peer.repository.check(list, milk.id, true)
        println("Checked Milk locally while offline")
        peer.rocky.setOnline(true)
        val converged = withTimeout(10_000) {
            creator.repository.observe(list).first { items -> items.any { it.id == milk.id && it.completed } }
        }
        println("Creator converged: $converged")
        creator.repository.deleteCategory(list, dairy)
        val uncategorized = withTimeout(10_000) {
            peer.repository.observe(list).first { items -> items.any { it.id == milk.id && it.category == null } }
        }
        println("Deleting the category kept the item: $uncategorized")
        println("Data retained in ${directory.absolutePath}; next run resumes the same domain")
    } finally {
        peer?.close()
        creator.close()
    }
}
