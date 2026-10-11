package rocky

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.internal.native.AppProtocol
import rocky.internal.native.NativeEndpoint
import rocky.internal.native.NativeException
import rocky.internal.native.ProtocolHandler
import rocky.internal.native.ByteStream as NativeByteStream

/**
 * An iroh endpoint and router the application owns. Its own protocols and Rocky's share it:
 * open a database with `PeerTransport.Endpoint(this)`. Closing that database leaves the endpoint
 * and its protocols running; `shutdown` is the application's last step. An endpoint created by
 * another library cannot be attached.
 */
class RockyEndpoint private constructor(internal val native: NativeEndpoint) : AppEndpoint {
    /** The endpoint's public key. It identifies a route, not a device's authority. */
    val id: String get() = native.id()

    /** The address other endpoints pass to `connect`. */
    fun address(): String = guarded { native.address() }

    /** A stream of the application protocol `protocol` to the endpoint at `address`. */
    suspend fun connect(address: String, protocol: String): ByteStream = try {
        NativeStream(native.connect(address, protocol))
    } catch (error: NativeException.Failure) {
        throw RockyException("unreachable", true, error.detail)
    }

    /** Closes the router and the endpoint. Databases still open on it lose their network only. */
    suspend fun shutdown() = withContext(NonCancellable) {
        try {
            native.shutdown()
        } catch (error: NativeException.Failure) {
            throw RockyException("storage", true, error.detail)
        } finally {
            native.destroy()
        }
    }
    private fun <T> guarded(call: () -> T): T = try {
        call()
    } catch (error: NativeException.Failure) {
        throw RockyException("storage", false, error.detail)
    }
    companion object {
        /**
         * Binds an endpoint identified by `secret`: 32 bytes the application keeps, so the address survives restarts.
         * `protocols` maps each application ALPN to the handler of one inbound stream; the stream closes when it returns.
         */
        suspend fun bind(
            secret: ByteArray,
            relay: String = "default",
            lookup: String = "default",
            topic: Boolean = false,
            local: Boolean = false,
            bind: String? = null,
            protocols: Map<String, suspend (remote: String, stream: ByteStream) -> Unit> = emptyMap(),
        ): RockyEndpoint = withContext(Dispatchers.IO) {
            val options = buildJsonObject {
                put("relay", relay)
                put("lookup", lookup)
                put("topic", topic)
                put("local", local)
                bind?.let { put("bind", it) }
            }
            val handlers = protocols.map { (name, handler) ->
                AppProtocol(
                    name,
                    object : ProtocolHandler {
                        override suspend fun accept(remote: String, stream: NativeByteStream) {
                            val wrapped = NativeStream(stream)
                            try {
                                handler(remote, wrapped)
                            } catch (cancelled: CancellationException) {
                                throw cancelled
                            } catch (_: Exception) {} finally {
                                (stream as? AutoCloseable)?.close()
                            }
                        }
                    },
                )
            }
            try {
                RockyEndpoint(NativeEndpoint.bind(options.toString(), secret, handlers))
            } catch (error: NativeException.Failure) {
                throw RockyException("storage", false, error.detail)
            }
        }
    }
}
