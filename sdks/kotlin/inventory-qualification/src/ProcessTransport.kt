package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.sync.*
import kotlinx.serialization.json.*
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicBoolean

/** JVM qualification transport. The host binds this connection to one authorized domain. */
class ProcessTransport private constructor(private val process: Process, val domain: JsonObject, private val reader: java.io.BufferedReader) : Transport {
    private val lifetime = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val writer = process.outputStream.bufferedWriter()
    private val writes = Mutex()
    private val budget = Semaphore(128)
    private val pending = ConcurrentHashMap<String, CompletableDeferred<JsonObject>>()
    private val sequence = AtomicLong()
    private val closed = AtomicBoolean(false)
    private val receive = lifetime.launch {
        try {
            while (true) {
                val line = reader.readLine() ?: break
                require(line.toByteArray().size <= 1024 * 1024 + 1024) { "host frame quota" }
                val message = Json.parseToJsonElement(line).jsonObject
                pending.remove(message.getValue("id").jsonPrimitive.content)?.complete(message.getValue("reply").jsonObject)
            }
        } finally {
            closed.set(true)
            pending.values.forEach { it.completeExceptionally(IllegalStateException("host closed")) }
            pending.clear()
        }
    }
    override suspend fun call(request: JsonObject): JsonObject {
        check(!closed.get()) { "transport closed" }
        if (!budget.tryAcquire()) throw RockyException("quota", true, "pending call quota")
        val id = sequence.incrementAndGet().toString()
        try {
            val reply = CompletableDeferred<JsonObject>()
            val call = buildJsonObject {
                put("id", id)
                put("call", buildJsonObject { put("version", 1); put("scope", domain); put("request", request) })
            }.toString()
            require(call.toByteArray().size <= 1024 * 1024) { "call frame quota" }
            pending[id] = reply
            check(!closed.get()) { "transport closed" }
            withContext(Dispatchers.IO) {
                writes.withLock { writer.write(call); writer.newLine(); writer.flush() }
            }
            val result = reply.await()
            check(result.getValue("version").jsonPrimitive.int == 1) { "SDK version mismatch" }
            if (result.getValue("status").jsonPrimitive.content == "error") {
                val error = result.getValue("error").jsonObject
                throw RockyException(error.getValue("code").jsonPrimitive.content,
                    error.getValue("retryable").jsonPrimitive.boolean, error.getValue("message").jsonPrimitive.content)
            }
            return result.getValue("value").jsonObject
        } finally {
            // Cancellation abandons the wait. Durable handles recover admitted work.
            pending.remove(id)
            budget.release()
        }
    }
    override suspend fun close() {
        withContext(NonCancellable + Dispatchers.IO) {
            closed.set(true)
            writes.withLock { writer.close() }
            try {
                withTimeout(10_000) {
                    receive.join()
                    check(runInterruptible { process.waitFor() } == 0) { "host failed" }
                }
            } finally {
                process.destroy()
                reader.close()
                lifetime.cancel()
            }
        }
    }
    companion object {
        suspend fun open(host: File, database: File, arguments: List<String> = emptyList()): ProcessTransport = withContext(Dispatchers.IO) {
            val process = ProcessBuilder(listOf(host.absolutePath, database.absolutePath) + arguments).redirectError(ProcessBuilder.Redirect.INHERIT).start()
            try {
                val reader = process.inputStream.bufferedReader()
                val hello = Json.parseToJsonElement(reader.readLine() ?: error("missing host greeting")).jsonObject
                check(hello.getValue("version").jsonPrimitive.int == 1)
                // Reuse the greeting reader: a buffered reader may read ahead.
                ProcessTransport(process, hello.getValue("scope").jsonObject, reader)
            } catch (error: Throwable) { process.destroy(); throw error }
        }
    }
}
