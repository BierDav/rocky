package rocky.build

import org.jetbrains.amper.plugins.*
import java.nio.file.Path
import kotlin.io.path.*

/**
 * Builds the native library, generates its UniFFI Kotlin bindings into [kotlin] and places the library
 * under the host's JNA resource prefix in [resources].
 */
@TaskAction
fun generateBindings(
    repositoryRoot: String,
    @Input rustSources: List<Path>,
    @Output kotlin: Path,
    @Output resources: Path,
) {
    val host = hostPlatform()
    val repository = Path(repositoryRoot)
    run(listOf("cargo", "build", "--manifest-path", "sdks/native/Cargo.toml", "--locked", "--features", "bindgen"), repository)
    val target = repository / "sdks/native/target/debug"
    run(listOf((target / "rocky-bindgen${host.executableSuffix}").pathString, "generate",
        "--library", (target / host.library).pathString, "--language", "kotlin",
        "--config", "sdks/native/uniffi.toml", "--out-dir", recreate(kotlin).pathString, "--no-format"), repository)
    val library = recreate(resources) / host.jnaPrefix / host.library
    (target / host.library).copyTo(library.createParentDirectories())
}
