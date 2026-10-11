@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, ExperimentalUnsignedTypes::class)

package org.example.examples.shopping.util

import kotlinx.cinterop.addressOf
import kotlinx.cinterop.convert
import kotlinx.cinterop.usePinned
import platform.CoreCrypto.CC_SHA256
import platform.CoreCrypto.CC_SHA256_DIGEST_LENGTH
import platform.Foundation.NSDate
import platform.Foundation.NSDateFormatter
import platform.Foundation.NSDateFormatterMediumStyle
import platform.Foundation.NSDateFormatterShortStyle
import platform.Foundation.dateWithTimeIntervalSince1970
import platform.Security.SecRandomCopyBytes
import platform.Security.kSecRandomDefault

internal actual fun randomBytes(size: Int): ByteArray = ByteArray(size).also { bytes ->
    if (size > 0) bytes.usePinned { check(SecRandomCopyBytes(kSecRandomDefault, size.toULong(), it.addressOf(0)) == 0) { "Secure random source failed" } }
}
internal actual fun formatDate(epochSeconds: Long): String = NSDateFormatter().apply {
    dateStyle = NSDateFormatterMediumStyle
    timeStyle = NSDateFormatterShortStyle
}.stringFromDate(NSDate.dateWithTimeIntervalSince1970(epochSeconds.toDouble()))
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
