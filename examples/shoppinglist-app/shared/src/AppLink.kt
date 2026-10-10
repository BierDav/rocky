package org.example.examples.shopping

/** The app's links. Query values use UTF-8 form encoding, including `+` for spaces. */
data class AppLink(val action: String, val parameters: Map<String, String> = emptyMap()) {
    override fun toString(): String = "$SCHEME://$action" +
        if (parameters.isEmpty()) "" else parameters.entries.joinToString("&", "?") { (name, value) -> "${encode(name)}=${encode(value)}" }

    companion object {
        const val SCHEME = "rocky-shopping"

        /** Null for another scheme or a malformed link. */
        fun parse(text: String): AppLink? = runCatching {
            val link = text.trim()
            if (!link.startsWith("$SCHEME://") || link.any { it.isWhitespace() }) return null
            val rest = link.removePrefix("$SCHEME://").substringBefore('#')
            val authority = rest.substringBefore('?').substringBefore('/')
            if (!authority.matches(Regex("[a-zA-Z0-9.-]+"))) return null
            val query = rest.substringAfter('?', "")
            val parameters = query.split('&').filter { it.isNotEmpty() }.associate {
                decode(it.substringBefore('=')) to decode(it.substringAfter('=', ""))
            }
            AppLink(authority, parameters)
        }.getOrNull()

        private fun encode(value: String): String = buildString {
            for (byte in value.encodeToByteArray()) {
                val n = byte.toInt() and 255
                when {
                    n == 32 -> append('+')

                    n.toChar() in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-*_" -> append(n.toChar())

                    else -> {
                        append('%')
                        append(HEX[n shr 4])
                        append(HEX[n and 15])
                    }
                }
            }
        }

        private fun decode(value: String): String {
            val bytes = mutableListOf<Byte>()
            var index = 0
            while (index < value.length) {
                when (value[index]) {
                    '%' -> {
                        require(index + 2 < value.length) { "Incomplete percent escape" }
                        bytes += ((value[index + 1].digitToInt(16) shl 4) or value[index + 2].digitToInt(16)).toByte()
                        index += 3
                    }

                    '+' -> {
                        bytes += 32.toByte()
                        index++
                    }

                    else -> {
                        val end = value.indexOfAny(charArrayOf('%', '+'), index).let { if (it < 0) value.length else it }
                        bytes += value.substring(index, end).encodeToByteArray().toList()
                        index = end
                    }
                }
            }
            return bytes.toByteArray().decodeToString()
        }
        private const val HEX = "0123456789ABCDEF"
    }
}
