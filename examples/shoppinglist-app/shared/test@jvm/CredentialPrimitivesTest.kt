package org.example.examples.shopping

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.*
import java.io.File
import java.nio.file.Files
import kotlin.test.*

/** The credential calls travel through the generated UniFFI session ABI. */
class CredentialPrimitivesTest {
    @Test fun `numbered nodes write every granted table and continue after reopen`() = runBlocking<Unit> {
        val root = Files.createTempDirectory("rocky-node-authors").toFile()
        val definition = schema("authors") {
            table("items") {
                text("title")
                primaryKey("id")
            }
            table("notes") {
                text("title")
                primaryKey("id")
            }
        }
        var issuer: RockyDatabase? = null
        var writer: RockyDatabase? = null
        try {
            val a = RockyDatabase.local(File(root, "a"), definition).also { issuer = it }
            val keys = Credentials.prepare(File(root, "w"))
            val writes = listOf("items", "notes").map { Grant(it, setOf(Right.Read, Right.Insert, Right.Custody)) }
            val supplied = a.credentials.grant(keys, writes, authors = 2)
            val w = RockyDatabase.install(File(root, "w"), definition, supplied).also { writer = it }
            assertFalse(w.isIssuer)
            for (table in listOf("items", "notes")) {
                w.write {
                    val id = nextId(table)
                    execute("INSERT INTO $table(id,title) VALUES(?, 'offline')", id)
                }
            }
            val originalNode = w.session().node
            w.close()
            writer = RockyDatabase.open(File(root, "w"), definition)
            assertEquals(originalNode, writer!!.session().node)
        } finally {
            writer?.close()
            issuer?.close()
            root.deleteRecursively()
        }
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
            val versionNow = current.getValue("version").jsonPrimitive.int
            val refusedVersions = (0..versionNow + 1).filter { it != versionNow } + Int.MAX_VALUE
            for (version in refusedVersions) {
                val bytes = JsonObject(current + ("version" to JsonPrimitive(version))).toString().toByteArray()
                identity.writeBytes(bytes)
                val error = assertFailsWith<RockyException> { RockyDatabase.open(directory, ItemsSchema) }
                assertTrue(error.message.orEmpty().contains("unsupported identity version"))
                assertContentEquals(bytes, identity.readBytes())
                assertContentEquals(kept, database.readBytes())
            }
        } finally {
            root.deleteRecursively()
        }
    }
}
