package org.example.examples.shopping.data.session

import eu.anifantakis.lib.ksafe.KSafe
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.builtins.nullable
import kotlinx.serialization.builtins.serializer
import rocky.SecretStore
import kotlin.io.encoding.Base64

/**
 * Rocky's secrets in KSafe: encrypted at rest and, once read, answered from memory, since Rocky reads them often
 * on its own thread. Writes and deletes return once durable, because Rocky uses a secret right after storing it.
 * The values live in the app's own files, so a reinstall starts without them.
 */
class SafeSecretStore(private val safe: KSafe) : SecretStore {
    override fun load(name: String): ByteArray? = safe.getDirect(name, null, String.serializer().nullable)?.let { Base64.decode(it) }

    override fun store(name: String, secret: ByteArray) = runBlocking {
        safe.put(name, Base64.encode(secret), String.serializer().nullable)
    }

    override fun delete(name: String) = runBlocking { safe.delete(name) }

    /** Forgets every secret, so the next installation creates fresh ones. */
    suspend fun clear() = safe.clearAll()

    companion object {
        /** The KSafe store each platform opens for Rocky. */
        const val FILE_NAME = "rocky_secrets"
    }
}
