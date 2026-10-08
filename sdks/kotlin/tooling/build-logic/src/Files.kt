package rocky.build

import java.net.URI
import java.nio.file.Path
import java.security.MessageDigest
import kotlin.io.path.*

fun sha256(path: Path): String =
    MessageDigest.getInstance("SHA-256").digest(path.readBytes()).joinToString("") { "%02x".format(it) }

/** Writes `SHA256.json`, the digest of every other file under [root]. */
@OptIn(ExperimentalPathApi::class)
fun writeManifest(root: Path) {
    val files = root.walk().filter { it.isRegularFile() && it.name != "SHA256.json" }
        .map { it.relativeTo(root).invariantSeparatorsPathString to sha256(it) }.sortedBy { it.first }.toList()
    val entries = files.joinToString(",\n") { (name, digest) -> "  \"$name\": \"$digest\"" }
    (root / "SHA256.json").writeText("{\n$entries\n}\n")
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
