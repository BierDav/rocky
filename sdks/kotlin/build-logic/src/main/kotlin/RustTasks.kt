import org.gradle.api.DefaultTask
import org.gradle.api.Project
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.*
import kotlin.io.path.*

/** The repository checkout that holds this SDK. */
val Project.repositoryRoot get() = rootProject.layout.projectDirectory.dir("../..")

/** The Rust sources the native runtime is built from, as task inputs. */
fun Project.rustSources(): List<Any> = listOf(
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "vendor",
    "crates",
    "sdks/native/Cargo.toml",
    "sdks/native/src",
    "sdks/uniffi/Cargo.toml",
    "sdks/uniffi/build.rs",
    "sdks/uniffi/Cargo.lock",
    "sdks/uniffi/uniffi.toml",
    "sdks/uniffi/askama.toml",
    "sdks/uniffi/src",
).map { repositoryRoot.file(it) }

/** A Cargo task over [rustSources]; Cargo itself decides what to rebuild. */
abstract class CargoTask : DefaultTask() {
    @get:Internal abstract val repository: DirectoryProperty

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sources: ConfigurableFileCollection

    @get:Internal
    protected val root get() = repository.get().asFile.toPath()

    protected fun cargo(vararg arguments: String, environment: Map<String, String> = emptyMap()) = run(listOf("cargo", *arguments), root, environment)
}

/**
 * Builds the host runtime and generates the UniFFI bindings from it: the common ABI, the JNA
 * implementation, the cinterop implementation and its C header, plus the host library as a JNA resource.
 */
abstract class GenerateBindings : CargoTask() {
    @get:OutputDirectory abstract val common: DirectoryProperty

    @get:OutputDirectory abstract val jna: DirectoryProperty

    @get:OutputDirectory abstract val ios: DirectoryProperty

    @get:OutputDirectory abstract val headers: DirectoryProperty

    @get:OutputDirectory abstract val hostLibrary: DirectoryProperty

    @TaskAction
    fun generate() {
        val host = hostPlatform()
        cargo("build", "--manifest-path", "sdks/uniffi/Cargo.toml", "--locked", "--features", "bindgen")
        val target = root / "sdks/uniffi/target/debug"
        val generated = recreate(common.get().asFile.toPath())
        run(
            listOf((target / "rocky-bindgen${host.executableSuffix}").pathString, "generate-native", (target / host.library).pathString, generated.pathString),
            root,
        )
        val jna = recreate(jna.get().asFile.toPath())
        (generated / "jna/rocky/internal/native/rocky_uniffi.kt").moveTo(jna / "RockyJna.kt")
        // Keep platform implementations out of the common source set.
        @OptIn(ExperimentalPathApi::class)
        (generated / "jna").deleteRecursively()
        (generated / "RockyNative.kt").moveTo(recreate(ios.get().asFile.toPath()) / "RockyNative.kt")
        (generated / "rocky_uniffiFFI.h").moveTo(recreate(headers.get().asFile.toPath()) / "rocky_uniffiFFI.h")
        val library = recreate(hostLibrary.get().asFile.toPath()) / host.jnaPrefix / host.library
        (target / host.library).copyTo(library.createParentDirectories())
    }
}

/** Builds the static runtime for one iOS target. */
abstract class BuildIosLibrary : CargoTask() {
    @get:Input abstract val target: Property<String>

    /** Adds the fault injection the in-repository simulator qualification uses; never for published builds. */
    @get:Input abstract val qualification: Property<Boolean>

    @get:OutputDirectory abstract val library: DirectoryProperty

    @TaskAction
    fun build() {
        val features = if (qualification.get()) arrayOf("--features", "qualification") else emptyArray()
        cargo(
            "build", "--manifest-path", "sdks/uniffi/Cargo.toml", "--lib", "--locked", "--target", target.get(), *features,
            environment = mapOf("IPHONEOS_DEPLOYMENT_TARGET" to IOS_DEPLOYMENT_TARGET),
        )
        val output = recreate(library.get().asFile.toPath())
        (root / "sdks/uniffi/target" / target.get() / "debug/librocky_uniffi.a").copyTo(output / "librocky_uniffi.a")
    }

    companion object {
        const val IOS_DEPLOYMENT_TARGET = "13.0"
    }
}

/** Builds the Android runtime and JNA's dispatch library for every packaged ABI, as `jniLibs`. */
abstract class BuildAndroidRuntime : CargoTask() {
    @get:Internal abstract val cache: DirectoryProperty

    @get:OutputDirectory abstract val libraries: DirectoryProperty

    @TaskAction
    fun build() = Android.buildRuntime(root, cache.get().asFile.toPath(), libraries.get().asFile.toPath())
}
