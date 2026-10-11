import org.gradle.api.DefaultTask
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.*
import java.io.File
import java.nio.file.Files
import kotlin.io.path.*

/** A standalone JVM library directory and a consumer compiled against it; see docs/runtime/sdk-packaging.md. */
abstract class PackageJvmSdk : DefaultTask() {
    /** The SDK's runtime jars, without the consumer. */
    @get:Classpath abstract val libraries: ConfigurableFileCollection

    @get:Classpath abstract val consumer: RegularFileProperty

    @get:InputFile
    @get:PathSensitive(PathSensitivity.NAME_ONLY)
    abstract val consumerSource: RegularFileProperty

    @get:Input abstract val mainClass: Property<String>

    @get:OutputDirectory abstract val output: DirectoryProperty

    @TaskAction
    fun pack() {
        val output = recreate(output.get().asFile.toPath())
        val lib = (output / "lib").createDirectories()
        for (file in libraries.files) {
            check(file.isFile) { "$file is not a jar; the package needs jar dependencies" }
            file.toPath().copyTo(lib / file.name)
        }
        consumer.get().asFile.toPath().copyTo(output / "consumer.jar")
        consumerSource.get().asFile.toPath().let { it.copyTo(output / it.name) }
        (output / "run").writeText(
            """
            |#!/bin/sh
            |set -eu
            |cd "${'$'}(CDPATH= cd -- "${'$'}(dirname -- "${'$'}0")" && pwd)"
            |exec java -cp 'consumer.jar:lib/*' ${mainClass.get()} "${'$'}@"
            |
            """.trimMargin(),
        )
        (output / "run").toFile().setExecutable(true)
        writeInventory(output)
    }
}

/** Runs the packaged consumer from a copy outside the repository, with only a JVM on `PATH`. */
@UntrackedTask(because = "Qualifies the package on every run")
abstract class QualifyJvmPackage : DefaultTask() {
    @get:InputDirectory abstract val packaged: DirectoryProperty

    @get:Input abstract val mainClass: Property<String>

    @get:Input abstract val java: Property<String>

    @OptIn(ExperimentalPathApi::class)
    @TaskAction
    fun qualify() {
        verifyInventory(packaged.get().asFile.toPath())
        val consumer = Files.createTempDirectory("rocky-consumer")
        packaged.get().asFile.toPath().copyToRecursively(consumer, followLinks = false, overwrite = false)
        val java = Path(java.get()).toRealPath()
        val runtime = (consumer / "runtime").createDirectories()
        (runtime / "java").createSymbolicLinkPointingTo(java)
        run(
            listOf(java.pathString, "-cp", "consumer.jar${File.pathSeparator}lib/*", mainClass.get(), (consumer / "data").pathString),
            consumer,
            mapOf("PATH" to runtime.pathString),
            clearEnvironment = true,
        )
        logger.lifecycle("Clean consumer: $consumer")
    }
}
