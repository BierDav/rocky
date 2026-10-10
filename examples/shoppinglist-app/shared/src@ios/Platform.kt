@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, ExperimentalUnsignedTypes::class)

package org.example.examples.shopping

import kotlinx.cinterop.addressOf
import kotlinx.cinterop.convert
import kotlinx.cinterop.usePinned
import platform.CoreCrypto.CC_SHA256
import platform.CoreCrypto.CC_SHA256_DIGEST_LENGTH
import platform.Foundation.NSData
import platform.Foundation.NSDate
import platform.Foundation.NSDateFormatter
import platform.Foundation.NSDateFormatterMediumStyle
import platform.Foundation.NSDateFormatterShortStyle
import platform.Foundation.NSFileManager
import platform.Foundation.create
import platform.Foundation.dateWithTimeIntervalSince1970
import platform.Foundation.writeToFile
import platform.Security.SecRandomCopyBytes
import platform.Security.kSecRandomDefault
import platform.posix.memcpy

internal actual fun randomBytes(size: Int): ByteArray = ByteArray(size).also { bytes ->
    if (size > 0) bytes.usePinned { check(SecRandomCopyBytes(kSecRandomDefault, size.toULong(), it.addressOf(0)) == 0) { "Secure random source failed" } }
}
internal actual fun moveDirectory(source: String, target: String): Boolean = NSFileManager.defaultManager.moveItemAtPath(source, target, null)
internal actual fun removeDirectory(path: String) {
    val files = NSFileManager.defaultManager
    if (files.fileExistsAtPath(path)) check(files.removeItemAtPath(path, null)) { "Could not remove incomplete household" }
}
internal actual fun formatDate(epochSeconds: Long): String = NSDateFormatter().apply {
    dateStyle = NSDateFormatterMediumStyle
    timeStyle = NSDateFormatterShortStyle
}.stringFromDate(NSDate.dateWithTimeIntervalSince1970(epochSeconds.toDouble()))
internal actual fun readText(path: String): String? {
    val data = NSFileManager.defaultManager.contentsAtPath(path) ?: return null
    val bytes = ByteArray(data.length.toInt())
    if (bytes.isNotEmpty()) bytes.usePinned { memcpy(it.addressOf(0), data.bytes, data.length) }
    return bytes.decodeToString()
}
internal actual fun replaceText(path: String, text: String) {
    val bytes = text.encodeToByteArray()
    val data = if (bytes.isEmpty()) NSData() else bytes.usePinned { NSData.create(bytes = it.addressOf(0), length = bytes.size.convert()) }
    check(data.writeToFile(path, atomically = true)) { "Could not save $path" }
}
internal actual fun sha256(bytes: ByteArray): ByteArray {
    val digest = UByteArray(CC_SHA256_DIGEST_LENGTH)
    digest.usePinned { out ->
        if (bytes.isEmpty()) {
            CC_SHA256(null, 0u, out.addressOf(0))
        } else {
            bytes.usePinned { CC_SHA256(it.addressOf(0), bytes.size.convert(), out.addressOf(0)) }
        }
    }
    return digest.asByteArray()
}
