package rocky.build

import org.jetbrains.amper.plugins.*
import java.io.File
import java.nio.file.Path
import kotlin.io.path.*

@Configurable
interface Qualification {
    /** Class whose `main` receives a fresh work directory and the repository root. */
    val mainClass: String

    /** Cargo arguments run from the repository root before the main class, such as building a host. */
    val prepare: List<String> get() = emptyList()

    /** Cargo arguments run after the main class, followed by its work directory. */
    val verify: List<String> get() = emptyList()

    /** Sources that must each be rejected by the type checker. */
    val rejected: Rejected?
}

@Configurable
interface Rejected {
    /** Directory of fixtures; each is compiled alone with [with] and must fail with a type mismatch. */
    val fixtures: Path

    /** Sources each fixture is compiled with. */
    val with: List<Path> get() = emptyList()
}

@TaskAction(executionAvoidance = ExecutionAvoidance.Disabled)
fun qualify(
    @Input classpath: Classpath,
    @Input jar: CompilationArtifact,
    @Input compileClasspath: Classpath,
    @Input rejected: Rejected?,
    @Output work: Path,
    repositoryRoot: String,
    mainClass: String,
    prepare: List<String>,
    verify: List<String>,
) {
    val repository = Path(repositoryRoot)
    if (prepare.isNotEmpty()) run(listOf("cargo") + prepare, repository)
    rejected?.let { reject(it, compileClasspath.resolvedFiles, recreate(work / "rejected")) }
    val runtime = (listOf(jar.artifact) + classpath.resolvedFiles).joinToString(File.pathSeparator)
    val directory = recreate(work / "run")
    run(listOf(javaLauncher(), "-cp", runtime, mainClass, directory.pathString, repository.pathString), repository)
    if (verify.isNotEmpty()) run(listOf("cargo") + verify + directory.pathString, repository)
}

private fun reject(rejected: Rejected, classpath: List<Path>, output: Path) {
    val fixtures = rejected.fixtures.listDirectoryEntries("*.kt").sorted()
    check(fixtures.isNotEmpty()) { "no rejected fixtures in ${rejected.fixtures}" }
    for (fixture in fixtures) {
        val compilation = compileKotlin(rejected.with + listOf(fixture), classpath, output)
        check(!compilation.succeeded) { "$fixture compiled; it must be a type error" }
        check("mismatch" in compilation.messages) { "$fixture failed for another reason:\n${compilation.messages}" }
        println("rejected ${fixture.name}")
    }
}
