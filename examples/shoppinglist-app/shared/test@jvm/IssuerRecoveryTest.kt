package org.example.examples.shopping

import java.io.File
import java.net.ServerSocket
import java.nio.file.Files
import kotlin.test.*
import kotlinx.coroutines.*
import rocky.*

/** The starter pairing makes a second issuer, which carries the list once the creator device is lost. */
class IssuerRecoveryTest {
    private fun tcp(): PeerTransport = PeerTransport.Tcp(ServerSocket(0).use { "127.0.0.1:${it.localPort}" })
    private val writer = GrantProfile.Writer(setOf("shopping_items"))
    private val rooms = mutableMapOf<RockyDatabase, ShoppingDatabase>()
    private fun RockyDatabase.withRoom(directory: File) = also { rooms[it] = openShoppingDatabase(directory, it) }
    private suspend fun RockyDatabase.add(title: String) = ShoppingRepository(rooms.getValue(this)).add(title)
    private suspend fun RockyDatabase.titles() = rooms.getValue(this).items().all().map { it.title }.sorted()
    private suspend fun close(runtime: RockyDatabase) { rooms.remove(runtime)?.close(); runtime.close() }
    private suspend fun join(directory: File, invitation: PairingInvitation) =
        RockyDatabase.join(directory, ShoppingSchema, invitation.uri, tcp(), automaticSync = false)

    @Test fun `a second device invites and renews after the creator device is destroyed`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-issuers").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, tcp(), automaticSync = false).withRoom(File(root, "a")).also { opened += it }
            a.installShoppingAuthor()
            val b = join(File(root, "b"), a.pairDevice()).withRoom(File(root, "b")).also { opened += it }
            assertTrue(b.isIssuer)
            assertEquals(GrantProfile.Issuer, b.session().profile)
            assertNotEquals(a.session().node, b.session().node)
            assertNotEquals(a.session().principal, b.session().principal)
            // A credential the creator issued for one minute, to a device that knows both issuers.
            val keys = Credentials.prepare(File(root, "d"))
            val supplied = a.grant(keys, writer, ttlSeconds = 60)
            val d = RockyDatabase.install(File(root, "d"), ShoppingSchema, supplied, transport = tcp()).withRoom(File(root, "d")).also { opened += it }
            val expiring = d.session()
            a.add("Milk")
            assertEquals(ConnectionState.Connected, d.syncOnce().state)
            assertEquals(ConnectionState.Connected, b.syncOnce().state)
            assertEquals(listOf("Milk"), b.titles())

            // The creator device is destroyed, keys included; the second issuer revokes it.
            val creator = a.session().node
            close(a); opened -= a
            assertTrue(File(root, "a").deleteRecursively())
            b.revoke(creator)
            assertEquals(expiring.policyEpoch + 1, b.session().policyEpoch)
            assertTrue(b.peers().none { it.node == creator })

            val c = join(File(root, "c"), b.pairing(writer)).withRoom(File(root, "c")).also { opened += it }
            assertEquals(ConnectionState.Connected, c.syncOnce().state)
            assertEquals(listOf("Milk"), c.titles())

            val renewed = d.renew()
            assertEquals(b.session().policyEpoch, renewed.policyEpoch)
            assertTrue(renewed.expiresAtEpochSeconds > expiring.expiresAtEpochSeconds + 24 * 60 * 60)
            assertTrue(d.peers().none { it.node == creator })
            // The second issuer assigns the authors a device inserts under.
            d.installShoppingAuthor(issuer = b)
            d.add("Eggs")
            assertEquals(ConnectionState.Connected, d.syncOnce().state)
            assertEquals(ConnectionState.Connected, c.syncOnce().state)
            assertEquals(listOf("Eggs", "Milk"), c.titles())
        } finally { opened.forEach { close(it) }; root.deleteRecursively() }
    }

    @Test fun `a device paired with the opt-out cannot issue`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-issuers").toFile()
        val opened = mutableListOf<RockyDatabase>()
        try {
            val a = RockyDatabase.create(File(root, "a"), ShoppingSchema, tcp(), automaticSync = false).withRoom(File(root, "a")).also { opened += it }
            val x = join(File(root, "x"), a.pairDevice(issuing = false)).withRoom(File(root, "x")).also { opened += it }
            assertFalse(x.isIssuer)
            assertFailsWith<RockyException> { x.installShoppingAuthor() }
            x.installShoppingAuthor(issuer = a)
            assertEquals(GrantProfile.Writer(setOf("shopping_items")), x.session().profile)
            val keys = Credentials.prepare(File(root, "y"))
            assertFailsWith<RockyException> { x.pairDevice() }
            assertFailsWith<RockyException> { x.pairing(writer) }
            assertFailsWith<RockyException> { x.grant(keys, writer) }
            assertFailsWith<RockyException> { x.delegate(keys, writer) }
            assertFailsWith<RockyException> { x.revoke(a.session().node) }
            x.add("Milk")
            assertEquals(ConnectionState.Connected, x.syncOnce().state)
            assertEquals(listOf("Milk"), a.titles())
            // The creator still holds the only publisher key and keeps its logout guard.
            assertFailsWith<RockyException> { a.logout() }
        } finally { opened.forEach { close(it) }; root.deleteRecursively() }
    }
}
