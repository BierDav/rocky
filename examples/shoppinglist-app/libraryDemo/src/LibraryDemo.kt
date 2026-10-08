package org.example.examples.shopping

import java.io.File
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.GrantProfile

/** A developer walkthrough; the full application UI is deferred. */
fun main() = runBlocking {
    val directory = File(requireNotNull(System.getenv("ROCKY_DEMO_DATA")))
    val creatorDirectory = File(directory, "creator")
    val creator = if (File(creatorDirectory, "identity.json").exists())
        ShoppingDesktop.resume(creatorDirectory)
    else ShoppingDesktop.create(creatorDirectory)
    var peer: ShoppingDesktop? = null
    try {
        val invitation = creator.runtime.pairing(GrantProfile.Writer(setOf("shopping_items")))
        println("Transport: ${creator.runtime.session().transport}; QR payload: ${invitation.uri}")
        val peerDirectory = File(directory, "peer")
        peer = if (File(peerDirectory, "identity.json").exists())
            ShoppingDesktop.resume(peerDirectory)
        else ShoppingDesktop.join(peerDirectory, invitation.uri)
        creator.repository.add("Milk")
        val rows = withTimeout(10_000) {
            peer.repository.observe().first { items -> items.any { it.title == "Milk" && !it.completed } }
        }
        println("Live Room query on peer: $rows")
        val milk = rows.first { it.title == "Milk" && !it.completed }
        peer.runtime.setOnline(false)
        peer.repository.check(milk.id, true)
        withTimeout(10_000) {
            peer.repository.observe().first { items -> items.any { it.id == milk.id && it.completed } }
        }
        println("Checked Milk locally while offline")
        peer.runtime.setOnline(true)
        val converged = withTimeout(10_000) {
            creator.repository.observe().first { items -> items.any { it.id == milk.id && it.completed } }
        }
        println("Creator converged: $converged")
        println("Data retained in ${directory.absolutePath}; next run resumes the same domain")
    } finally { peer?.close(); creator.close() }
}
