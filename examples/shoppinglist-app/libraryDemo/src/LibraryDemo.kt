package org.example.examples.shopping

import java.io.File
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.GrantProfile

/** A developer walkthrough of a shared list; the Compose screen is in [ShoppingScreen]. */
fun main() = runBlocking {
    val directory = File(requireNotNull(System.getenv("ROCKY_DEMO_DATA")))
    val creatorDirectory = File(directory, "creator")
    val creator = if (File(creatorDirectory, "identity.json").exists())
        ShoppingDesktop.resume(creatorDirectory)
    else ShoppingDesktop.create(creatorDirectory)
    var peer: ShoppingDesktop? = null
    try {
        val peerDirectory = File(directory, "peer")
        peer = if (File(peerDirectory, "identity.json").exists())
            ShoppingDesktop.resume(peerDirectory)
        else {
            val invitation = creator.runtime.pairing(GrantProfile.Writer(ShoppingTables.toSet()))
            println("Transport: ${creator.runtime.session().transport}; QR payload: ${invitation.uri}")
            ShoppingDesktop.join(peerDirectory, invitation.uri).also { it.runtime.installShoppingAuthor(issuer = creator.runtime) }
        }
        val list = creator.repository.createList("Groceries")
        // One envelope shares the list with its categories and items.
        val node = peer.runtime.session().node
        peer.repository.accept(list, creator.repository.share(list, listOf(node)).getValue(node))
        val dairy = creator.repository.addCategory(list, "Dairy")
        creator.repository.add(list, "Milk", category = dairy)
        val rows = withTimeout(10_000) {
            peer.repository.observe(list).first { items -> items.any { it.title == "Milk" && !it.completed } }
        }
        println("Live Room query on peer: $rows")
        val milk = rows.first { it.title == "Milk" && !it.completed }
        peer.runtime.setOnline(false)
        peer.repository.check(list, milk.id, true)
        println("Checked Milk locally while offline")
        peer.runtime.setOnline(true)
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
    } finally { peer?.close(); creator.close() }
}
