package rocky.testing
class TestFile(val path: String) {
    constructor(parent: TestFile, name: String) : this(parent.path + "/" + name)
    val absolutePath: String get() = path
    val parentFile: TestFile? get() = path.substringBeforeLast('/', "").takeIf { it.isNotEmpty() }?.let(::TestFile)
    val isFile: Boolean get() = testExists(path)
    fun exists(): Boolean = testExists(path)
    fun readText(): String = testRead(path)
    fun readLines(): List<String> = readText().lines()
    fun writeText(text: String) = testWrite(path, text)
    fun deleteRecursively() = testRemove(path)
    fun toFile(): TestFile = this
}
object TestFiles {
    fun createTempDirectory(prefix: String): TestFile = TestFile(testTemp(prefix))
}
expect fun testRepositoryRoot(): String
expect fun testEnvironment(name: String): String?
internal expect fun testExists(path: String): Boolean
internal expect fun testRead(path: String): String
internal expect fun testWrite(path: String, text: String)
internal expect fun testRemove(path: String)
internal expect fun testTemp(prefix: String): String
