package org.example.examples.shopping

import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder

/**
 * The app's links, `rocky-shopping://<action>?<name>=<value>&…`. Android opens them in the app, so a phone's
 * camera app hands a scanned pairing code straight to it. Values are URL-encoded.
 */
data class AppLink(val action: String, val parameters: Map<String, String> = emptyMap()) {
    override fun toString(): String = "$SCHEME://$action" +
        if (parameters.isEmpty()) "" else parameters.entries.joinToString("&", "?") { (name, value) -> "${encode(name)}=${encode(value)}" }

    companion object {
        const val SCHEME = "rocky-shopping"
        /** Null when [text] is not one of the app's links. */
        fun parse(text: String): AppLink? {
            val uri = runCatching { URI(text.trim()) }.getOrNull() ?: return null
            val action = uri.host ?: return null
            if (uri.scheme != SCHEME) return null
            val parameters = uri.rawQuery.orEmpty().split("&").filter { it.isNotEmpty() }
                .associate { decode(it.substringBefore("=")) to decode(it.substringAfter("=", "")) }
            return AppLink(action, parameters)
        }
        // The charset-name overloads exist on every Android version the app supports.
        private fun encode(value: String) = URLEncoder.encode(value, "UTF-8")
        private fun decode(value: String) = URLDecoder.decode(value, "UTF-8")
    }
}
