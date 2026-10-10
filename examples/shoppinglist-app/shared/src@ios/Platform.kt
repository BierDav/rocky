@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package org.example.examples.shopping

import kotlinx.cinterop.addressOf
import kotlinx.cinterop.usePinned
import platform.Foundation.NSDate
import platform.Foundation.NSDateFormatter
import platform.Foundation.NSDateFormatterMediumStyle
import platform.Foundation.NSDateFormatterShortStyle
import platform.Foundation.NSFileManager
import platform.Foundation.dateWithTimeIntervalSince1970
import platform.Security.SecRandomCopyBytes
import platform.Security.kSecRandomDefault

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
