package org.example.examples.shopping.util

import io.github.vinceglb.filekit.PlatformFile
import io.github.vinceglb.filekit.atomicMove
import io.github.vinceglb.filekit.delete
import io.github.vinceglb.filekit.exists
import io.github.vinceglb.filekit.isRegularFile
import io.github.vinceglb.filekit.readString
import io.github.vinceglb.filekit.writeString
import kotlinx.coroutines.CancellationException

internal fun epochMillis(): Long = kotlin.time.Clock.System.now().toEpochMilliseconds()
internal expect fun randomBytes(size: Int): ByteArray
internal expect fun formatDate(epochSeconds: Long): String

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

internal expect fun sha256(bytes: ByteArray): ByteArray
internal fun ByteArray.hex(): String = joinToString("") { (it.toInt() and 0xff).toString(16).padStart(2, '0') }
