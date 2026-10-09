package rocky.build

import org.jetbrains.amper.plugins.*
import java.nio.file.Path
import kotlin.io.path.*

/** One task generates the common ABI, UniFFI JNA and Rocky's cinterop implementation. */
@TaskAction
fun generateBindings(
    repositoryRoot: String,
    @Input rustSources: List<Path>,
    @Output common: Path,
    @Output jna: Path,
    @Output ios: Path,
    @Output resources: Path,
    @Output cinterop: Path,
    @Output simulatorDef: Path,
    @Output deviceDef: Path,
) {
    val host = hostPlatform()
    val repository = Path(repositoryRoot)
    run(listOf("cargo", "build", "--manifest-path", "sdks/native/Cargo.toml", "--locked", "--features", "bindgen"), repository)
    val target = repository / "sdks/native/target/debug"
    val generated = recreate(common)
    run(listOf((target / "rocky-bindgen${host.executableSuffix}").pathString, "generate-native", (target / host.library).pathString, generated.pathString), repository)
    recreate(jna)
    (generated / "jna/rocky/internal/native/rocky_uniffi.kt").copyTo(jna / "RockyJna.kt")
    recreate(ios)
    (generated / "RockyNative.kt").moveTo(ios / "RockyNative.kt")
    // Keep platform implementations out of the common fragment.
    (generated / "jna").toFile().deleteRecursively()
    val header = generated / "rocky_uniffiFFI.h"
    val definition = recreate(cinterop)
    header.moveTo(definition / header.name)
    val def = """
        headers = rocky_uniffiFFI.h malloc/malloc.h
        headerFilter = rocky_uniffiFFI.h malloc/malloc.h
        package = rocky_uniffi.cinterop
        compilerOpts = -I${definition.pathString}
        staticLibraries = librocky_uniffi.a
        libraryPaths = ${repository / "sdks/native/target/aarch64-apple-ios-sim/debug"}
        linkerOpts = -framework CoreFoundation -framework Security -framework SystemConfiguration -framework Network -liconv
    """.trimIndent()
    simulatorDef.writeText(def)
    deviceDef.writeText(def.replace("apple-ios-sim", "apple-ios"))
    val library = recreate(resources) / host.jnaPrefix / host.library
    (target / host.library).copyTo(library.createParentDirectories())
}

/** Builds only the static target archive the consuming iOS fragment needs. */
@TaskAction
fun buildIosBinding(
    repositoryRoot: String,
    target: String,
    @Input definition: Path,
    @Input rustSources: List<Path>,
    @Output defFile: Path,
) {
    run(
        listOf("cargo", "build", "--manifest-path", "sdks/native/Cargo.toml", "--lib", "--target", target, "--locked"),
        Path(repositoryRoot),
        mapOf("IPHONEOS_DEPLOYMENT_TARGET" to "13.0"),
    )
    definition.copyTo(defFile.createParentDirectories(), overwrite = true)
}
