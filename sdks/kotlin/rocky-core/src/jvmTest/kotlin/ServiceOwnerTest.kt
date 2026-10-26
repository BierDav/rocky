package rocky

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import java.io.File
import java.net.DatagramSocket
import java.net.ServerSocket
import java.nio.file.Files
import java.util.concurrent.TimeUnit
import kotlin.random.Random
import kotlin.test.*

class ServiceOwnerTest {
    @Test
    fun closingAtTheTenantQuotaAllowsAnotherDomainAndRefusesANonOwner() = runBlocking<Unit> {
        val repository = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }
            .first { File(it, "rust-toolchain.toml").exists() }
        val directory = Files.createTempDirectory("rocky-service-close").toFile()
        val voters = mutableListOf<Process>()
        val databases = mutableListOf<RockyDatabase>()
        try {
            fun command(vararg arguments: String): String {
                val output = File(directory, "command.log")
                val process = ProcessBuilder(*arguments).directory(repository).redirectErrorStream(true).redirectOutput(output).start()
                check(process.waitFor(180, TimeUnit.SECONDS)) {
                    process.destroyForcibly()
                    "service command timed out"
                }
                check(process.exitValue() == 0) { output.readText() }
                return output.readText()
            }
            command("cargo", "build", "--locked", "-p", "rocky-coordinator", "--bin", "rocky-voter")
            val voter = File(repository, "target/debug/rocky-voter").absolutePath
            val service = File(directory, "service")
            command(voter, "local", service.absolutePath, availablePortBlock().toString())
            for (id in 1..3) {
                voters += ProcessBuilder(voter, File(service, "voter-$id.json").absolutePath, "--provision")
                    .redirectErrorStream(true).redirectOutput(File(directory, "voter-$id.log")).start()
            }
            val admin = File(service, "admin.json").absolutePath
            withTimeout(30_000) {
                while (runCatching { command(voter, "admin", admin, "initialize") }.isFailure) delay(20)
                while (runCatching {
                        Json.parseToJsonElement(command(voter, "admin", admin, "status", "1")).jsonObject["leader"] != JsonNull
                    }.getOrDefault(false).not()
                ) {
                    delay(20)
                }
            }
            val definition = schema("closure") {
                table("stock") {
                    text("key")
                    primaryKey("key")
                }
            }
            val runtime = ServiceCoordinator(File(service, "service/bundle.json").readText())
            val network = PeerTransport.Iroh.isolated()
            suspend fun create(name: String): RockyDatabase = RockyDatabase.create(
                File(directory, name),
                definition,
                network,
                automaticSync = false,
                coordinator = runtime,
            ).also { databases += it }
            val tenant = Random.nextBytes(32)
            val first = create("domain-0")
            suspend fun register(database: RockyDatabase, id: String): ServiceControl {
                val identity = database.session().node
                return database.service(tenant).register(id, listOf(identity))
            }
            val firstRegistration = newMutationHandle()
            val accepted = register(first, firstRegistration)
            val signed = Json.parseToJsonElement(File(directory, "domain-0/tenant.requests").readText()).jsonArray.single().jsonObject
            assertEquals(signed.getValue("time").jsonPrimitive.long + 86_400, accepted.expiresAtEpochSeconds)
            assertEquals(accepted, register(first, firstRegistration))
            for (index in 1 until 16) {
                val database = create("domain-$index")
                register(database, newMutationHandle())
                database.close()
                databases.remove(database)
            }
            val owner = first.service(tenant)
            assertEquals(16, owner.usage().openDomains)
            val next = create("domain-16")
            val registration = newMutationHandle()
            assertEquals("quota", assertFailsWith<RockyException> { register(next, registration) }.code)
            val invitation = first.pairing(GrantProfile.Writer(setOf("stock")))
            val member = RockyDatabase.join(File(directory, "member"), definition, invitation.uri, network, automaticSync = false, coordinator = runtime)
                .also { databases += it }
            assertFalse(member.isIssuer)
            assertEquals("denied", assertFailsWith<RockyException> { member.service(Random.nextBytes(32)).close(newMutationHandle()) }.code)
            assertEquals(0, owner.usage().closedDomains)
            val close = newMutationHandle()
            val closed = owner.close(close)
            assertEquals(closed, owner.close(close))
            assertEquals(15, owner.usage().openDomains)
            assertEquals(1, owner.usage().closedDomains)
            assertTrue(owner.usage().bytes > 0)
            register(next, registration)
            assertEquals(16, owner.usage().openDomains)
            assertEquals("denied", assertFailsWith<RockyException> { register(first, newMutationHandle()) }.code)
        } finally {
            databases.asReversed().forEach { it.close() }
            voters.forEach { it.destroyForcibly().waitFor() }
            directory.deleteRecursively()
        }
    }

    private fun availablePortBlock(): Int {
        repeat(32) {
            val base = Random.nextInt(10_000, 60_000)
            val sockets = mutableListOf<AutoCloseable>()
            try {
                for (id in 1..3) {
                    sockets += ServerSocket(base + id)
                    sockets += DatagramSocket(base + 10 + id)
                }
                return base
            } catch (_: java.net.BindException) {
                // Retry a different block while holding every port checked so far.
            } finally {
                sockets.forEach { it.close() }
            }
        }
        error("no free voter port block")
    }
}
