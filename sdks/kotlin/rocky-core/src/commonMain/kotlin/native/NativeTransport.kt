package rocky

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.internal.native.NativeException
import rocky.internal.native.NativeSecretStore
import rocky.internal.native.ByteStream as NativeByteStream

/** A native stream as Kotlin uses it. */
internal class NativeStream(private val stream: NativeByteStream) : ByteStream {
    override suspend fun read(max: Int): ByteArray = guarded { stream.read(max.toUInt()) }
    override suspend fun write(bytes: ByteArray) = guarded { stream.write(bytes) }
    override suspend fun finish() = guarded { stream.finish() }
    override suspend fun close() {
        try {
            stream.abandon()
        } finally {
            (stream as? AutoCloseable)?.close()
        }
    }
    private suspend fun <T> guarded(call: suspend () -> T): T = try {
        call()
    } catch (error: NativeException.Failure) {
        throw RockyException("unreachable", true, error.detail)
    }
}

/** In-process coordinated log for development. A restart loses its ledger. */
class DevelopmentLog {
    internal val native = rocky.internal.native.NativeDevelopmentLog()
    fun configure(database: RockyDatabase, minimum: Int = 1) {
        require(minimum > 0)
        native.configure(database.domain, minimum.toUInt())
    }
    fun close() = native.destroy()
}

/** A metadata coordination service pinned by its serialized current trust bundle. */
class ServiceCoordinator(val trustBundle: String)

/** A store's failure reaches the runtime as an error with its message; it is never read as an absent secret. */
internal fun secretProvider(secrets: SecretStore?): NativeSecretStore? = secrets?.let { foreign ->
    object : NativeSecretStore {
        override fun load(name: String): ByteArray? = guarded { foreign.load(name) }
        override fun store(name: String, secret: ByteArray) = guarded { foreign.store(name, secret) }
        override fun delete(name: String) = guarded { foreign.delete(name) }
        private fun <T> guarded(call: () -> T): T = try {
            call()
        } catch (error: Exception) {
            throw NativeException.Failure("secret store: ${error.message ?: error}")
        }
    }
}
