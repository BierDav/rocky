package org.example.examples.shopping

import java.io.File
import java.security.SecureRandom
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

internal actual fun randomBytes(size: Int): ByteArray = ByteArray(size).also(SecureRandom()::nextBytes)
internal actual fun moveDirectory(source: String, target: String): Boolean = File(source).renameTo(File(target))
internal actual fun removeDirectory(path: String) {
    check(File(path).deleteRecursively()) { "Could not remove incomplete household" }
}
internal actual fun formatDate(epochSeconds: Long): String = DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
    .withZone(ZoneId.systemDefault()).format(Instant.ofEpochSecond(epochSeconds))
