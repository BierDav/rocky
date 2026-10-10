package org.example.examples.shopping

import java.io.File
import java.security.MessageDigest
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
internal actual fun readText(path: String): String? = File(path).takeIf { it.isFile }?.readText()
internal actual fun replaceText(path: String, text: String) {
    val target = File(path).absoluteFile
    val pending = File.createTempFile(".rocky-", ".tmp", target.parentFile)
    try {
        pending.writeText(text)
        check(pending.renameTo(target)) { "Could not save $path" }
    } finally {
        pending.delete()
    }
}
internal actual fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)
