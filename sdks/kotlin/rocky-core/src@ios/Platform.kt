@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class)

package rocky
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.usePinned
import platform.Foundation.NSFileManager
import platform.Security.SecRandomCopyBytes
import platform.Security.kSecRandomDefault
internal actual fun secureBytes(size: Int): ByteArray = ByteArray(size).also { bytes ->
    bytes.usePinned { check(SecRandomCopyBytes(kSecRandomDefault, size.toULong(), it.addressOf(0)) == 0) { "secure random source failed" } }
}
internal actual fun fileExists(path: String): Boolean = NSFileManager.defaultManager.fileExistsAtPath(path)
actual open class PlatformOpeners
actual open class PlatformCredentialOpeners
