package org.example.examples.shopping.util

import io.github.vinceglb.filekit.PlatformFile
import io.github.vinceglb.filekit.atomicMove
import io.github.vinceglb.filekit.delete
import io.github.vinceglb.filekit.exists
import io.github.vinceglb.filekit.isRegularFile
import io.github.vinceglb.filekit.readString
import io.github.vinceglb.filekit.writeString
import kotlinx.coroutines.CancellationException
import kotlinx.datetime.LocalDate
import kotlinx.datetime.LocalDateTime
import kotlinx.datetime.TimeZone
import kotlinx.datetime.format.char
import kotlinx.datetime.toLocalDateTime
import org.kotlincrypto.hash.sha2.SHA256
import org.kotlincrypto.random.CryptoRand
import kotlin.time.Instant

internal fun epochMillis(): Long = kotlin.time.Clock.System.now().toEpochMilliseconds()
internal fun randomBytes(size: Int): ByteArray = CryptoRand.Default.nextBytes(ByteArray(size))
internal fun formatDate(epochSeconds: Long, timeZone: TimeZone = TimeZone.currentSystemDefault()): String = dateFormat.format(Instant.fromEpochSeconds(epochSeconds).toLocalDateTime(timeZone))

/** Local event time preserves the SDK timestamp's millisecond precision. */
internal fun formatTimestamp(epochMillis: Long, timeZone: TimeZone = TimeZone.currentSystemDefault()): String = Instant.fromEpochMilliseconds(epochMillis).toLocalDateTime(timeZone).toString().replace('T', ' ')

private val dateFormat = LocalDateTime.Format {
    date(LocalDate.Formats.ISO)
    char(' ')
    hour()
    char(':')
    minute()
}

internal suspend fun moveDirectory(source: String, target: String): Boolean = try {
    PlatformFile(source).atomicMove(PlatformFile(target))
    true
} catch (cancelled: CancellationException) {
    throw cancelled
} catch (_: Exception) {
    false
}

internal suspend fun removeDirectory(path: String) {
    PlatformFile(path).delete(recursively = true, mustExist = false)
}

internal suspend fun removeFile(path: String) {
    PlatformFile(path).delete(mustExist = false)
}

/** The file's text, or null when it does not exist. */
internal suspend fun readText(path: String): String? {
    val file = PlatformFile(path)
    return if (file.exists() && file.isRegularFile()) file.readString() else null
}

/** Replaces the file with [text] in one rename. */
internal suspend fun replaceText(path: String, text: String) {
    val target = PlatformFile(path)
    val pending = PlatformFile("$path.tmp-${randomBytes(8).hex()}")
    try {
        pending.writeString(text)
        pending.atomicMove(target)
    } finally {
        pending.delete(mustExist = false)
    }
}

internal fun sha256(bytes: ByteArray): ByteArray = SHA256().digest(bytes)
internal fun ByteArray.hex(): String = joinToString("") { (it.toInt() and 0xff).toString(16).padStart(2, '0') }
