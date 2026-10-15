package rocky

import kotlinx.coroutines.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** An application-supplied identity: one invitation enrolls it, its further nodes certify themselves. */
class IdentityTest {
    @Test
    fun aSecondNodeOfAnIdentityJoinsWithoutAnIssuerAndRevokingTheIdentityStopsBoth() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-identity").toFile()
        val definition = schema("identities") {
            table("items") {
                text("title")
                primaryKey("id")
            }
        }
        val person = FileIdentityProvider(File(root, "person").absolutePath)
        val owner = RockyDatabase.create(File(root, "owner"), definition, PeerTransport.Iroh.isolated(), automaticSync = false)
        val opened = mutableListOf<RockyDatabase>(owner)
        try {
            val writer = GrantProfile.Writer(setOf("items"))
            val first = RockyDatabase.join(File(root, "first"), definition, owner.pairing(writer).uri, PeerTransport.Iroh.isolated(), automaticSync = false, identity = person)
            opened += first
            // The second node asks the first, not an issuer.
            val second = RockyDatabase.enrollIdentity(File(root, "second"), definition, first.contact().uri, FileIdentityProvider(File(root, "person").absolutePath), PeerTransport.Iroh.isolated(), automaticSync = false)
            opened += second
            val key = person.publicKey().joinToString("") { "%02x".format(it) }
            val (a, b) = first.session() to second.session()
            assertEquals(key, a.principal)
            assertEquals(key, b.principal)
            assertNotEquals(a.node, b.node)
            assertFalse(b.isIssuer)
            // Revoking the identity ends both nodes; the owner's own identity is never revoked.
            owner.revokeIdentity(key)
            assertEquals(1uL, owner.session().revocations.statements)
            val refused = assertFailsWith<RockyException> { owner.revokeIdentity(owner.session().principal) }
            assertEquals("rejected", refused.code)
        } finally {
            opened.reversed().forEach { it.close() }
            root.deleteRecursively()
        }
    }
}
