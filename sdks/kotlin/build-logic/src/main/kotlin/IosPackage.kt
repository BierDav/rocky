import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.*
import java.io.ByteArrayOutputStream
import kotlin.io.path.*

/**
 * The iOS SDK as one directory: the Maven repository, the runtime as a static XCFramework, build
 * facts and the SHA-256 inventory of every file; see docs/runtime/ios.md.
 */
abstract class PackageIos : DefaultTask() {
    @get:InputDirectory abstract val repository: DirectoryProperty

    @get:InputDirectory abstract val deviceLibrary: DirectoryProperty

    @get:InputDirectory abstract val simulatorLibrary: DirectoryProperty

    @get:InputDirectory abstract val headers: DirectoryProperty

    @get:Input abstract val version: Property<String>

    @get:Input abstract val kotlinVersion: Property<String>

    @get:OutputDirectory abstract val output: DirectoryProperty

    @OptIn(ExperimentalPathApi::class)
    @TaskAction
    fun pack() {
        val output = recreate(output.get().asFile.toPath())
        val source = repository.get().asFile.toPath()
        // Maven bookkeeping is not an artifact and must not reach the registry.
        source.copyToRecursively(output / "maven", followLinks = false) { from, to ->
            if (!from.name.startsWith("maven-metadata")) from.copyToIgnoringExistingDirectory(to, followLinks = false)
            CopyActionResult.CONTINUE
        }
        run(
            listOf(
                "xcodebuild", "-create-xcframework",
                "-library", deviceLibrary.get().file("librocky_uniffi.a").asFile.path, "-headers", headers.get().asFile.path,
                "-library", simulatorLibrary.get().file("librocky_uniffi.a").asFile.path, "-headers", headers.get().asFile.path,
                "-output", (output / "RockyRuntime.xcframework").pathString,
            ),
            output,
        )
        val xcode = ByteArrayOutputStream().also { log ->
            val process = ProcessBuilder("xcodebuild", "-version").redirectErrorStream(true).start()
            process.inputStream.copyTo(log)
            check(process.waitFor() == 0) { "xcodebuild -version failed" }
        }.toString().trim().replace("\n", "\\n")
        (output / "BUILD.json").writeText(
            """
            |{
            |  "version": "${version.get()}",
            |  "minimumIos": "${BuildIosLibrary.IOS_DEPLOYMENT_TARGET}",
            |  "kotlin": "${kotlinVersion.get()}",
            |  "xcode": "$xcode",
            |  "profile": "debug",
            |  "runtimeEmbeddedInCinterop": true
            |}
            |
            """.trimMargin(),
        )
        writeInventory(output)
        logger.lifecycle("$output")
    }
}

/** Verifies the package inventory, then builds and tests the standalone consumer with Rust tools absent from `PATH`. */
@UntrackedTask(because = "Qualifies the package on every run")
abstract class QualifyIosPackage : DefaultTask() {
    @get:InputDirectory abstract val packaged: DirectoryProperty

    /** The standalone consumer build. */
    @get:Internal abstract val consumer: DirectoryProperty

    @get:Internal abstract val gradlew: RegularFileProperty

    @get:InputFile abstract val entitlements: RegularFileProperty

    @get:Input abstract val version: Property<String>

    @get:Input abstract val javaHome: Property<String>

    @get:Optional @get:Input
    abstract val developerDirectory: Property<String>

    @TaskAction
    fun qualify() {
        val packaged = packaged.get().asFile.toPath()
        verifyInventory(packaged)
        logger.lifecycle("iOS inventory verified")
        val environment = mutableMapOf(
            // Standard macOS/Xcode tools only; Gradle provisions Kotlin/Native itself.
            "PATH" to "/usr/bin:/bin:/usr/sbin:/sbin",
            "JAVA_HOME" to javaHome.get(),
            "HOME" to System.getProperty("user.home"),
        )
        developerDirectory.orNull?.let { environment["DEVELOPER_DIR"] = it }
        check(listOf("cargo", "rustup").none { name -> environment.getValue("PATH").split(':').any { Path(it, name).exists() } }) {
            "qualification PATH contains Rust tooling"
        }
        run(
            listOf(
                gradlew.get().asFile.path, "-p", consumer.get().asFile.path, "--no-configuration-cache",
                "-Procky.repository=${packaged / "maven"}", "-ProckyVersion=${version.get()}",
                "-Procky.entitlements=${entitlements.get().asFile.path}",
                "linkDebugTestIosArm64", "iosSimulatorArm64Test",
            ),
            packaged,
            environment,
            clearEnvironment = true,
        )
    }
}
