@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, kotlinx.cinterop.BetaInteropApi::class)

package rocky.testing
import kotlinx.cinterop.toKString
import platform.Foundation.NSFileManager
import platform.Foundation.NSString
import platform.Foundation.NSTemporaryDirectory
import platform.Foundation.NSUTF8StringEncoding
import platform.Foundation.NSUUID
import platform.Foundation.create
import platform.Foundation.stringWithContentsOfFile
import platform.Foundation.writeToFile
import platform.posix.getenv
actual fun testRepositoryRoot(): String = getenv("ROCKY_TEST_ROOT")?.toKString() ?: error("ROCKY_TEST_ROOT must name the source fixture directory")
actual fun testEnvironment(name: String): String? = getenv(name)?.toKString()
internal actual fun testExists(path: String): Boolean = NSFileManager.defaultManager.fileExistsAtPath(path)
internal actual fun testRead(path: String): String = NSString.stringWithContentsOfFile(path, NSUTF8StringEncoding, null) ?: error("cannot read fixture $path")
internal actual fun testWrite(path: String, text: String) {
    check(NSString.create(string = text).writeToFile(path, true, NSUTF8StringEncoding, null))
}
internal actual fun testRemove(path: String) {
    NSFileManager.defaultManager.removeItemAtPath(path, null)
}
internal actual fun testTemp(prefix: String): String = (NSTemporaryDirectory() + prefix + NSUUID().UUIDString).also { check(NSFileManager.defaultManager.createDirectoryAtPath(it, true, null, null)) }
