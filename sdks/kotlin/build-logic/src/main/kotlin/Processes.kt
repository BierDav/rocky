import java.io.File
import java.nio.file.Path

/** Runs [command] in [directory], streaming its output into the task log; fails on a non-zero exit. */
fun run(
    command: List<String>,
    directory: Path,
    environment: Map<String, String> = emptyMap(),
    clearEnvironment: Boolean = false,
) {
    val process = ProcessBuilder(command).directory(directory.toFile()).redirectErrorStream(true).apply {
        if (clearEnvironment) environment().clear()
        environment().putAll(environment)
    }.start()
    process.inputStream.bufferedReader().forEachLine(::println)
    val exit = process.waitFor()
    check(exit == 0) { "${command.joinToString(" ")} exited with $exit" }
}

/** The executable [command] on `PATH`, if any. */
fun onPath(command: String): String? = System.getenv("PATH").orEmpty().split(File.pathSeparator).map { File(it, command) }.firstOrNull(File::canExecute)?.path
