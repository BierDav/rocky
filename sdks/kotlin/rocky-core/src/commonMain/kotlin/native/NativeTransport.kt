package rocky

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.internal.native.ForeignTransport
import rocky.internal.native.NativeCommandHandler
import rocky.internal.native.NativeCommands
import rocky.internal.native.NativeException
import rocky.internal.native.NativeHandlerReads
import rocky.internal.native.NativeSecretStore
import rocky.internal.native.OutboundStream
import rocky.internal.native.ByteStream as NativeByteStream
import rocky.internal.native.InboundStream as NativeInboundStream

/** A Kotlin stream as the native runtime calls it. Unexpected failures of `close` are swallowed. */
internal class ForeignStream(private val stream: ByteStream) : NativeByteStream {
    override suspend fun read(max: UInt): ByteArray = guarded { stream.read(max.toInt()) }
    override suspend fun write(bytes: ByteArray) = guarded { stream.write(bytes) }
    override suspend fun finish() = guarded { stream.finish() }
    override suspend fun abandon() {
        try {
            stream.close()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {}
    }
    private suspend fun <T> guarded(call: suspend () -> T): T = try {
        call()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Exception) {
        throw NativeException.Failure(error.message ?: error.toString())
    }
}

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
internal class ForeignAdapter(private val transport: StreamTransport) : ForeignTransport {
    override fun kind(): String = transport.kind
    override suspend fun address(): String = guarded { transport.address() }
    override suspend fun connect(address: String, protocol: String): OutboundStream = guarded { OutboundStream(ForeignStream(transport.connect(address, protocol))) }
    override suspend fun accept(): NativeInboundStream? = guarded { transport.accept()?.let { NativeInboundStream(it.protocol, ForeignStream(it.stream)) } }
    private suspend fun <T> guarded(call: suspend () -> T): T = try {
        call()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (error: Exception) {
        throw NativeException.Failure(error.message ?: error.toString())
    }
}

/**
 * An in-process coordinator for development and tests. Its ledger is in memory: every database that shares it
 * lives in this process, and a domain it confirmed for makes no strong progress after the process ends.
 * It sees identities and commitments, never commands, arguments or rows.
 */
class DevelopmentCoordinator : CommandCoordinator {
    fun close() = native.destroy()
    internal val native = rocky.internal.native.DevelopmentCoordinator()

    /** Registers `owner`'s domain with that device as first executor. `executors` may take over; `callers` may invoke. */
    fun register(owner: CommandIdentity, executors: List<CommandIdentity> = emptyList(), callers: List<CommandIdentity> = emptyList()) = guarded { native.register(owner.json, executors.map { it.json }, callers.map { it.json }) }

    /** Fences the current executor and assigns `executor`. */
    fun takeover(owner: CommandIdentity, executor: CommandIdentity) = guarded { native.takeover(owner.json, executor.json) }

    /** Activates verified local generation evidence at the exact coordinator head. */
    fun activate(owner: CommandIdentity, activation: JsonObject): JsonObject = guarded {
        Json.parseToJsonElement(native.activate(owner.json, activation.toString())).jsonObject
    }

    /** The number of confirmed transitions of `identity`'s domain. */
    fun sequence(identity: CommandIdentity): Long = guarded { native.sequence(identity.json).toLong() }

    /** Fault controls: an unreachable coordinator, and one that accepts and loses its answer. */
    fun setAvailable(available: Boolean) = guarded { native.setAvailable(available) }
    fun dropNextReply() = guarded { native.dropNextReply() }
    private fun <T> guarded(call: () -> T): T = try {
        call()
    } catch (error: NativeException.Failure) {
        throw RockyException("rejected", false, error.detail)
    }
}

/** A provider-operated or self-hosted coordinator pinned by its serialized trust bundle. */
class ServiceCoordinator(val trustBundle: String) : CommandCoordinator

internal fun composeCommands(commands: CommandRuntime?): NativeCommands? = commands?.let { runtime ->
    val coordinator = runtime.coordinator
    require(coordinator is DevelopmentCoordinator || coordinator is ServiceCoordinator)
    val dispatcher = runtime.handlers.takeIf { it.isNotEmpty() }?.let(::CommandDispatcher)
    val handler = dispatcher?.let { dispatcher ->
        object : NativeCommandHandler {
            override fun handle(input: String, reads: NativeHandlerReads): String = try {
                dispatcher.handle(
                    input,
                    JsonCommandReads(reads::get) { table, lower, upper, limit -> reads.scan(table, lower, upper, limit.toUInt()) },
                )
            } finally {
                reads.destroy()
            }
        }
    }
    NativeCommands(
        (coordinator as? DevelopmentCoordinator)?.native,
        (coordinator as? ServiceCoordinator)?.trustBundle,
        handler,
        runtime.handlerDeadlineMillis?.toULong(),
    )
}

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
