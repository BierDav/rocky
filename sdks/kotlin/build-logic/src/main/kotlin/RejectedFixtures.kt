import org.gradle.api.DefaultTask
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.*
import org.gradle.process.ExecOperations
import java.io.ByteArrayOutputStream
import java.io.File
import javax.inject.Inject

/** Compiles each fixture alone with [support] against [classpath]; every one must fail with a type mismatch. */
abstract class RejectedFixtures @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val fixtures: ConfigurableFileCollection

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val support: ConfigurableFileCollection

    @get:Classpath abstract val classpath: ConfigurableFileCollection

    /** The embeddable Kotlin compiler. */
    @get:Classpath abstract val compiler: ConfigurableFileCollection

    @get:OutputDirectory abstract val work: DirectoryProperty

    @TaskAction
    fun reject() {
        val sources = fixtures.files.sorted()
        check(sources.isNotEmpty()) { "no rejected fixtures" }
        for (fixture in sources) {
            val log = ByteArrayOutputStream()
            val result = exec.javaexec {
                classpath(compiler)
                mainClass.set("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
                args(support.files.map(File::getPath) + fixture.path)
                args("-no-stdlib", "-no-reflect", "-jvm-target", "17", "-d", work.get().asFile.path)
                args("-cp", this@RejectedFixtures.classpath.asPath)
                standardOutput = log
                errorOutput = log
                isIgnoreExitValue = true
            }
            val messages = log.toString()
            check(result.exitValue != 0) { "${fixture.name} compiled; it must be a type error" }
            check("mismatch" in messages) { "${fixture.name} failed for another reason:\n$messages" }
            logger.lifecycle("rejected ${fixture.name}")
        }
    }
}
