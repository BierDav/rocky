package org.example.examples.shopping

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import kotlin.concurrent.atomics.AtomicReference
import kotlin.concurrent.atomics.ExperimentalAtomicApi

/** Peer names chosen here; this file is never replicated. */
@OptIn(ExperimentalAtomicApi::class)
class ContactLabels(private val path: String) {
    private val names = AtomicReference(
        readText(path)?.let { text ->
            val saved = Json.parseToJsonElement(text).jsonObject
            require(saved["version"]?.jsonPrimitive?.int == 1) { "Unsupported local contact label format" }
            saved.getValue("names").jsonObject.also { labels ->
                require(labels.keys.all { it.length == 64 && it.all { char -> char in "0123456789abcdef" } })
            }
        } ?: JsonObject(emptyMap()),
    )
    private val saving = Mutex()

    private fun reference(node: String): String = sha256("rocky.shopping.contact-label.v1:$node".encodeToByteArray()).hex()

    fun label(node: String): String = names.load()[reference(node)]?.jsonPrimitive?.content ?: "Peer ${node.take(6)}"

    suspend fun name(node: String, label: String) = saving.withLock {
        require(label.encodeToByteArray().size <= 256) { "A local label holds at most 256 bytes" }
        val updated = JsonObject(names.load() + (reference(node) to JsonPrimitive(label.ifBlank { "Peer ${node.take(6)}" })))
        replaceText(
            path,
            buildJsonObject {
                put("version", 1)
                put("names", updated)
            }.toString(),
        )
        names.store(updated)
    }
}
