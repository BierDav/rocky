package org.example.examples.shopping

internal fun epochMillis(): Long = kotlin.time.Clock.System.now().toEpochMilliseconds()
internal expect fun randomBytes(size: Int): ByteArray
internal expect fun moveDirectory(source: String, target: String): Boolean
internal expect fun removeDirectory(path: String)
internal expect fun formatDate(epochSeconds: Long): String
