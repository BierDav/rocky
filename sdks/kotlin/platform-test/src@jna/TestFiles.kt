package rocky.testing
import java.io.File
import java.nio.file.Files
actual fun testRepositoryRoot(): String = generateSequence(File(requireNotNull(System.getProperty("user.dir")) { "test working directory is unavailable" })) { it.parentFile }.first { File(it, "project.yaml").isFile }.absolutePath
actual fun testEnvironment(name: String): String? = System.getenv(name)
internal actual fun testExists(path: String): Boolean = File(path).exists()
internal actual fun testRead(path: String): String = File(path).readText()
internal actual fun testWrite(path: String, text: String) = File(path).writeText(text)
internal actual fun testRemove(path: String) {
    File(path).deleteRecursively()
}
internal actual fun testTemp(prefix: String): String = Files.createTempDirectory(prefix).toString()
