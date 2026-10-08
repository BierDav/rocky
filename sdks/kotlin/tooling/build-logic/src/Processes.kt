package rocky.build

import java.io.File
import java.nio.file.Path

/** Runs [command] in [directory], streaming its output into the task log; fails on a non-zero exit. */
fun run(
    command: List<String>,
    directory: Path,
    environment: Map<String, String> = emptyMap(),
    clearEnvironment: Boolean = false,
) {
    val exit = start(command, directory, environment, clearEnvironment).apply {
        inputStream.bufferedReader().forEachLine(::println)
    }.waitFor()
    check(exit == 0) { "${command.joinToString(" ")} exited with $exit" }
}

/** Runs [command] and returns its combined output; fails with that output on a non-zero exit. */
fun capture(command: List<String>, directory: Path): String =
    start(command, directory, emptyMap(), false).let { process ->
        val output = process.inputStream.bufferedReader().readText()
        check(process.waitFor() == 0) { "${command.joinToString(" ")} failed:\n$output" }
        output
    }

private fun start(command: List<String>, directory: Path, environment: Map<String, String>, clear: Boolean): Process =
    ProcessBuilder(command).directory(directory.toFile()).redirectErrorStream(true).apply {
        if (clear) environment().clear()
        environment().putAll(environment)
    }.start()

/** The executable [command] on `PATH`, if any; child processes resolve commands against the parent's `PATH`. */
fun onPath(command: String): String? =
    System.getenv("PATH").orEmpty().split(File.pathSeparator).map { File(it, command) }.firstOrNull(File::canExecute)?.path

/** The `java` launcher of `JAVA_HOME`, else the one on `PATH`. */
fun javaLauncher(): String =
    System.getenv("JAVA_HOME")?.let { File(it, "bin/java").takeIf(File::canExecute)?.path } ?: "java"
