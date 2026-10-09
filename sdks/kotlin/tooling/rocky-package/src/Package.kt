package rocky.build

import org.jetbrains.amper.plugins.*
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import kotlin.io.path.*

/** A standalone JVM library directory and a consumer compiled against it; see docs/runtime/sdk-packaging.md. */
@TaskAction
fun packageSdk(
    @Input classpath: Classpath,
    @Input jar: CompilationArtifact,
    @Input consumerSource: Path,
    mainClass: String,
    @Output output: Path,
) {
    val lib = recreate(output).resolve("lib").createDirectories()
    // The runtime classpath includes the consumer itself, which is packaged beside the library.
    for (file in classpath.resolvedFiles.filter { it.name != jar.artifact.name }) {
        check(file.isRegularFile()) { "$file is not a jar; the package needs jar dependencies" }
        file.copyTo(lib / file.name)
    }
    jar.artifact.copyTo(output / "consumer.jar")
    consumerSource.copyTo(output / consumerSource.name)
    (output / "run").writeText(
        """
        |#!/bin/sh
        |set -eu
        |cd "${'$'}(CDPATH= cd -- "${'$'}(dirname -- "${'$'}0")" && pwd)"
        |exec java -cp 'consumer.jar:lib/*' $mainClass "${'$'}@"
        |
        """.trimMargin(),
    )
    (output / "run").toFile().setExecutable(true)
    writeManifest(output)
    println(output)
}

/** Runs the packaged consumer from a copy outside the repository, with only a JVM on `PATH`. */
@OptIn(ExperimentalPathApi::class)
@TaskAction(executionAvoidance = ExecutionAvoidance.Disabled)
fun qualifyPackage(@Input packaged: Path, mainClass: String) {
    val consumer = Files.createTempDirectory("rocky-consumer")
    packaged.copyToRecursively(consumer, followLinks = false, overwrite = false)
    val java = Path(javaLauncher().let { if (File(it).isAbsolute) it else onPath(it) ?: error("$it is not on PATH") }).toRealPath()
    val runtime = (consumer / "runtime").createDirectories()
    (runtime / "java").createSymbolicLinkPointingTo(java)
    run(
        listOf(java.pathString, "-cp", "consumer.jar${File.pathSeparator}lib/*", mainClass, (consumer / "data").pathString),
        consumer,
        mapOf("PATH" to runtime.pathString),
        clearEnvironment = true,
    )
    println("Clean consumer: $consumer")
}

/** Qualifies the package on linux/amd64 in Docker, from a copy of the tracked sources. */
@TaskAction(executionAvoidance = ExecutionAvoidance.Disabled)
fun qualifyPackageOnLinux(repositoryRoot: String) {
    val repository = Path(repositoryRoot)
    val qualification = repository / "local/packaged-linux"
    val source = recreate(qualification / "source")
    for (name in capture(listOf("jj", "file", "list"), repository).lines().filter(String::isNotEmpty)) {
        val file = repository / name
        if (file.isRegularFile()) file.copyTo((source / name).createParentDirectories(), overwrite = true)
    }
    (source / "kotlin").toFile().setExecutable(true)
    run(
        listOf(
            "docker", "run", "--rm", "--platform", "linux/amd64",
            "-v", "$source:/source",
            "-v", "${qualification / "cargo"}:/usr/local/cargo/registry",
            "-v", "${qualification / "cache"}:/root/.cache",
            "rust@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1", "sh", "-c",
            """
            set -eu
            apt-get update
            apt-get install -y --no-install-recommends openjdk-17-jdk-headless unzip curl pkg-config libclang-dev
            cd /source
            ./kotlin check packaged -m packaged-consumer
            """.trimIndent(),
        ),
        repository,
    )
}
