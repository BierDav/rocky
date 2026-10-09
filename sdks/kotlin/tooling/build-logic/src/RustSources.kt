package rocky.build

import java.nio.file.Path
import java.security.MessageDigest
import kotlin.io.path.*

/** The repository paths the native runtime is built from, relative to the repository root. */
val RUST_SOURCES = listOf(
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "crates",
    "sdks/native/Cargo.toml",
    "sdks/native/Cargo.lock",
    "sdks/native/src",
)

/** A digest of every file under [RUST_SOURCES]: their paths and contents, excluding Cargo `target` directories. */
@OptIn(ExperimentalPathApi::class)
fun rustSourcesDigest(repository: Path): String {
    val digest = MessageDigest.getInstance("SHA-256")
    RUST_SOURCES.map(repository::resolve).flatMap { root ->
        if (root.isRegularFile()) {
            listOf(root)
        } else {
            root.walk().filter { it.isRegularFile() && "target" !in it.relativeTo(repository).map(Path::name) }.toList()
        }
    }.map { it.relativeTo(repository).invariantSeparatorsPathString to it }.sortedBy { it.first }.forEach { (name, file) ->
        digest.update(name.toByteArray())
        digest.update(0)
        digest.update(file.readBytes())
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

/** The runtime AAR the `android-runtime` Gradle build publishes to Maven local. */
object RuntimeArtifact {
    const val GROUP = "rocky"
    const val NAME = "rocky-runtime-android"
    const val VERSION = "0.1.0"

    /** The POM property recording [rustSourcesDigest] of the sources it was built from. */
    const val DIGEST_PROPERTY = "rocky.rust-sources"

    fun directory(): Path = Path(System.getProperty("user.home"), ".m2/repository", GROUP.replace('.', '/'), NAME, VERSION)
}
