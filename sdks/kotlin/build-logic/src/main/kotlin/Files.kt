import java.net.URI
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.io.path.*

fun sha256(path: Path): String = MessageDigest.getInstance("SHA-256").digest(path.readBytes()).joinToString("") { "%02x".format(it) }

/** The digest of every file under [root] except `SHA256.json`, by relative path. */
@OptIn(ExperimentalPathApi::class)
fun inventory(root: Path): Map<String, String> = root.walk().filter { it.isRegularFile() && it.name != "SHA256.json" }
    .map { it.relativeTo(root).invariantSeparatorsPathString to sha256(it) }.sortedBy { it.first }.toMap()

/** Writes `SHA256.json`, the [inventory] of [root]. */
fun writeInventory(root: Path) {
    val entries = inventory(root).entries.joinToString(",\n") { (name, digest) -> "  \"$name\": \"$digest\"" }
    (root / "SHA256.json").writeText("{\n$entries\n}\n")
}

/** Fails unless `SHA256.json` names exactly the files under [root] and their digests. */
fun verifyInventory(root: Path) {
    val expected = Regex(""""([^"]+)": "([0-9a-f]+)"""").findAll((root / "SHA256.json").readText())
        .associate { it.groupValues[1] to it.groupValues[2] }
    check(inventory(root) == expected) { "$root does not match its SHA256.json inventory" }
}

/** Downloads [url] to [target] once and verifies its pinned [digest]. */
fun fetch(url: String, target: Path, digest: String): Path {
    if (!target.exists()) {
        target.createParentDirectories()
        val partial = target.resolveSibling(target.name + ".download")
        URI(url).toURL().openStream().use { input -> partial.outputStream().use(input::copyTo) }
        partial.moveTo(target, overwrite = true)
    }
    if (sha256(target) != digest) {
        target.deleteExisting()
        error("checksum mismatch for $url")
    }
    return target
}

/** Empties [directory], creating it if needed. */
@OptIn(ExperimentalPathApi::class)
fun recreate(directory: Path): Path {
    directory.deleteRecursively()
    return directory.createDirectories()
}
