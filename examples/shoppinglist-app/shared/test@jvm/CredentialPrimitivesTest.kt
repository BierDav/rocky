package org.example.examples.shopping

import java.io.File
import java.nio.file.Files
import kotlin.test.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.*

/** The credential calls travel through the generated UniFFI session ABI. */
class CredentialPrimitivesTest {
    @Test fun `pseudonyms are assigned installed mapped and renewed through native calls`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-pseudonyms").toFile()
        val schema = schema("authors") {
            table("items") { text("title"); primaryKey("id") }
            table("notes") { text("title"); primaryKey("id") }
        }
        var issuer: RockyDatabase? = null
        var writer: RockyDatabase? = null
        var reader: RockyDatabase? = null
        var other: RockyDatabase? = null
        try {
            val a = RockyDatabase.local(File(root, "a"), schema).also { issuer = it }
            val keys = Credentials.prepare(File(root, "w"))
            val writes = listOf("items", "notes").map { Grant(it, setOf(Right.Read, Right.Insert, Right.Custody)) }
            val supplied = a.credentials.grant(keys, writes, authors = 2)
            val w = RockyDatabase.install(File(root, "w"), schema, supplied).also { writer = it }
            assertFalse(w.isIssuer)
            assertFailsWith<RockyException> { w.credentials.assignPseudonym(w.credentials.pseudonymRequest("items")) }
            val asked = w.credentials.pseudonymRequest("items")
            val credential = a.credentials.assignPseudonym(asked)
            val item = w.credentials.installPseudonym(credential)
            assertEquals(AuthorPseudonym("items", 3), item)
            assertEquals(item, w.credentials.installPseudonym(a.credentials.assignPseudonym(asked)))
            assertEquals(PseudonymOwner(w.session().node, "items", 3), a.credentials.pseudonymOwner(credential))
            assertFalse(credential.toString().contains(w.session().node))
            assertFalse(credential.toString().contains(a.session().principal))
            assertFailsWith<RockyException> { w.credentials.pseudonymOwner(credential) }
            assertFailsWith<RockyException> { a.credentials.installPseudonym(credential) }
            val noteCredential = a.credentials.assignPseudonym(w.credentials.pseudonymRequest("notes"))
            assertEquals(AuthorPseudonym("notes", 4), w.credentials.installPseudonym(noteCredential))
            // Each table uses a distinct signing key, bound in its credential leaf.
            assertNotEquals(credential.last().jsonObject["capability"]!!.jsonObject["subject"],
                noteCredential.last().jsonObject["capability"]!!.jsonObject["subject"])
            val otherKeys = Credentials.prepare(File(root, "other"))
            val recipient = RockyDatabase.install(File(root, "other"), schema,
                a.credentials.grant(otherKeys, writes)).also { other = it }
            for ((collection, number) in listOf("items" to 1, "notes" to 2)) {
                val assigned = w.credentials.assignPseudonym(recipient.credentials.pseudonymRequest(collection))
                assertEquals(AuthorPseudonym(collection, number), recipient.credentials.installPseudonym(assigned))
            }
            w.close(); writer = null
            val reopened = RockyDatabase.open(File(root, "w"), schema).also { writer = it }
            assertEquals(item, reopened.credentials.installPseudonym(a.credentials.assignPseudonym(asked)))
            // Removing and restoring supplied credentials cannot reset the allocator.
            reopened.credentials.install(a.credentials.grant(keys, writes))
            reopened.credentials.install(supplied)
            assertFailsWith<RockyException> { reopened.credentials.assignPseudonym(a.credentials.pseudonymRequest("items")) }
            assertEquals(item, reopened.credentials.installPseudonym(a.credentials.assignPseudonym(asked)))
            val rKeys = Credentials.prepare(File(root, "r"))
            val r = RockyDatabase.install(File(root, "r"), schema,
                a.credentials.grant(rKeys, listOf(Grant("items", setOf(Right.Read, Right.Custody))))).also { reader = it }
            assertFailsWith<RockyException> { a.credentials.assignPseudonym(r.credentials.pseudonymRequest("items")) }
            assertFailsWith<RockyException> { reopened.credentials.claimAuthorRange(1, 10) }
        } finally { other?.close(); reader?.close(); writer?.close(); issuer?.close(); root.deleteRecursively() }
    }

    @Test fun `other identity versions refuse through the native ABI without writes`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-identity-refusal").toFile()
        try {
            val directory = File(root, "a")
            RockyDatabase.local(directory, ItemsSchema).close()
            val identity = File(directory, "identity.json")
            val current = Json.parseToJsonElement(identity.readText()).jsonObject
            val database = File(directory, "rocky.sqlite")
            val kept = database.readBytes()
            for (version in listOf(0, 1, 2, 3, 4, 5, 6, 7, 8, 10, Int.MAX_VALUE)) {
                val bytes = JsonObject(current + ("version" to JsonPrimitive(version))).toString().toByteArray()
                identity.writeBytes(bytes)
                val error = assertFailsWith<RockyException> { RockyDatabase.open(directory, ItemsSchema) }
                assertTrue(error.message.orEmpty().contains("unsupported identity version"))
                assertContentEquals(bytes, identity.readBytes())
                assertContentEquals(kept, database.readBytes())
            }
        } finally { root.deleteRecursively() }
    }
}
