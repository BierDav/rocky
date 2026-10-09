package rocky.build

import org.jetbrains.kotlin.cli.jvm.K2JVMCompiler
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.PrintStream
import java.nio.file.Path
import kotlin.io.path.pathString

/** The outcome of one embedded `kotlinc` run. */
class Compilation(val succeeded: Boolean, val messages: String)

/** Compiles [sources] for JVM 17 against exactly [classpath], with no implicit standard library; [extra] adds compiler arguments. */
fun compileKotlin(sources: List<Path>, classpath: List<Path>, destination: Path, extra: List<String> = emptyList()): Compilation {
    val log = ByteArrayOutputStream()
    val arguments = sources.map(Path::pathString) +
        listOf(
            "-no-stdlib",
            "-no-reflect",
            "-jvm-target",
            "17",
            "-cp",
            classpath.joinToString(File.pathSeparator),
            "-d",
            destination.pathString,
        ) +
        extra
    val exit = PrintStream(log).use { K2JVMCompiler().exec(it, *arguments.toTypedArray()) }
    return Compilation(exit.code == 0, log.toString())
}
