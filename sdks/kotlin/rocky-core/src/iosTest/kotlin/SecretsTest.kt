@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky

import kotlinx.atomicfu.locks.ReentrantLock
import kotlinx.atomicfu.locks.withLock
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.usePinned
import kotlinx.coroutines.runBlocking
import platform.Foundation.NSFileManager
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUUID
import platform.Foundation.getBytes
import kotlin.test.*

/** What the runtime does with an application-supplied store. */
class SecretsTest {
    @Test fun identityFilesContainNoSecretsAndAMissingSecretRefusesOpen() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-secrets-" + NSUUID().UUIDString
        val memory = MemorySecrets()
        val recorded = RecordingSecrets(memory)
        val definition = schema("secretsgate", 1) {
            table("items") {
                text("key")
                primaryKey("key")
            }
        }
        try {
            val database = RockyDatabase.local(root, definition, recorded)
            database.close()
            val reopened = RockyDatabase.open(root, definition, secrets = memory)
            reopened.close()
            val manager = NSFileManager.defaultManager
            val paths = manager.subpathsOfDirectoryAtPath(root, null) ?: error("cannot enumerate identity directory")
            for (relative in paths) {
                val data = manager.contentsAtPath("$root/$relative") ?: continue
                val bytes = ByteArray(data.length.toInt())
                if (bytes.isNotEmpty()) bytes.usePinned { data.getBytes(it.addressOf(0), data.length) }
                recorded.values().forEach { secret ->
                    assertFalse(containsSecret(bytes, secret), "secret in $relative")
                }
            }
            recorded.purge()
            val failure = assertFailsWith<RockyException> { RockyDatabase.open(root, definition, secrets = recorded) }
            assertTrue(failure.message.orEmpty().contains("is missing"), failure.message)
            assertTrue(recorded.values().isEmpty(), "open recreated a secret")
        } finally {
            recorded.purge()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }

    @Test fun logoutPurgeDeletesEverySecretOfTheIdentity() = runBlocking<Unit> {
        val root = NSTemporaryDirectory() + "rocky-purge-" + NSUUID().UUIDString
        val secrets = RecordingSecrets(MemorySecrets())
        val definition = schema("purgegate", 1) {
            table("items") {
                text("key")
                primaryKey("key")
            }
        }
        try {
            val network = PeerTransport.Iroh.isolated()
            val owner = RockyDatabase.create("$root/owner", definition, network, automaticSync = false)
            val db = try {
                RockyDatabase.join("$root/member", definition, owner.pairing(GrantProfile.Issuer).uri, network, secrets = secrets, automaticSync = false)
            } catch (error: Throwable) {
                owner.close()
                throw error
            }
            try {
                val names = secrets.names()
                assertTrue(names.isNotEmpty())
                db.credentials.logout(purge = true)
                assertTrue(secrets.values().isEmpty())
                names.forEach { assertNull(secrets.load(it)) }
            } finally {
                db.close()
                owner.close()
            }
        } finally {
            secrets.purge()
            NSFileManager.defaultManager.removeItemAtPath(root, null)
        }
    }
}

internal class MemorySecrets : SecretStore {
    private val lock = ReentrantLock()
    private val held = mutableMapOf<String, ByteArray>()
    override fun load(name: String): ByteArray? = lock.withLock { held[name]?.copyOf() }
    override fun store(name: String, secret: ByteArray) = lock.withLock { held[name] = secret.copyOf() }
    override fun delete(name: String) = lock.withLock {
        held.remove(name)
        Unit
    }
}

internal class RecordingSecrets(private val store: SecretStore) : SecretStore {
    private val lock = ReentrantLock()
    private val recorded = mutableMapOf<String, ByteArray>()
    override fun load(name: String): ByteArray? = lock.withLock {
        store.load(name)?.also { recorded[name] = it.copyOf() }
    }
    override fun store(name: String, secret: ByteArray) = lock.withLock {
        store.store(name, secret)
        recorded[name] = secret.copyOf()
    }
    override fun delete(name: String) = lock.withLock {
        store.delete(name)
        recorded.remove(name)
        Unit
    }
    fun names(): List<String> = lock.withLock { recorded.keys.toList() }
    fun values(): List<ByteArray> = lock.withLock { recorded.values.map { it.copyOf() } }
    fun purge() = lock.withLock {
        recorded.keys.toList().forEach { store.delete(it) }
        recorded.clear()
    }
}

private fun containsSecret(bytes: ByteArray, secret: ByteArray): Boolean {
    var start = 0
    while (start <= bytes.size - secret.size) {
        var offset = 0
        while (offset < secret.size && bytes[start + offset] == secret[offset]) offset++
        if (offset == secret.size) return true
        start++
    }
    return false
}
