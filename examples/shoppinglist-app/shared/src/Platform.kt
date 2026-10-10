package org.example.examples.shopping

internal fun epochMillis(): Long = kotlin.time.Clock.System.now().toEpochMilliseconds()
internal expect fun randomBytes(size: Int): ByteArray
internal expect fun moveDirectory(source: String, target: String): Boolean
internal expect fun removeDirectory(path: String)
internal expect fun formatDate(epochSeconds: Long): String

/** The file's text, or null when it does not exist. */
internal expect fun readText(path: String): String?

/** Replaces the file with [text] in one rename. */
internal expect fun replaceText(path: String, text: String)
internal expect fun sha256(bytes: ByteArray): ByteArray
internal fun ByteArray.hex(): String = joinToString("") { (it.toInt() and 0xff).toString(16).padStart(2, '0') }
