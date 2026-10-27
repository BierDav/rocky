import org.gradle.testfixtures.ProjectBuilder
import java.nio.file.Files
import java.nio.file.NoSuchFileException
import kotlin.io.path.*
import kotlin.test.*

class RustTasksTest {
    @Test
    fun importsBindingsAndHostRuntimeWithoutCargo() = withProject { project ->
        val task = project.tasks.create("bindings", GenerateBindings::class.java)
        val source = project.layout.projectDirectory.dir("prebuilt")
        val outputs = listOf("common" to task.common, "jna" to task.jna, "ios" to task.ios, "headers" to task.headers, "host" to task.hostLibrary)
        for ((name, output) in outputs) {
            (source.asFile.toPath() / name / "nested/artifact").createParentDirectories().writeBytes(byteArrayOf(0, 1, 2, 3))
            output.set(project.layout.buildDirectory.dir(name))
            (output.get().asFile.toPath() / "stale").createParentDirectories().writeText("obsolete")
        }
        task.prebuilt.set(source)
        // No Cargo repository is configured: importing must not invoke a build.
        task.generate()
        for ((name, output) in outputs) {
            assertContentEquals((source.asFile.toPath() / name / "nested/artifact").readBytes(), (output.get().asFile.toPath() / "nested/artifact").readBytes())
            assertFalse((output.get().asFile.toPath() / "stale").exists())
        }
    }

    @Test
    fun refusesIncompleteBindings() = withProject { project ->
        val task = project.tasks.create("bindings", GenerateBindings::class.java)
        task.prebuilt.set(project.layout.projectDirectory.dir("prebuilt").also { it.asFile.mkdirs() })
        task.common.set(project.layout.buildDirectory.dir("common"))
        assertFailsWith<NoSuchFileException> { task.generate() }
    }

    @Test
    fun importsRuntimeWithoutCargoAndRemovesStaleOutputs() = withProject { project ->
        val task = project.tasks.create("runtime", BuildIosLibrary::class.java)
        val source = project.layout.projectDirectory.file("librocky_uniffi.a")
        source.asFile.writeBytes(byteArrayOf(0, 1, 2, 3))
        task.prebuilt.set(source)
        task.qualification.set(false)
        task.library.set(project.layout.buildDirectory.dir("runtime"))
        (task.library.get().asFile.toPath() / "stale").createParentDirectories().writeText("obsolete")
        task.build()
        assertContentEquals(source.asFile.readBytes(), task.library.get().file("librocky_uniffi.a").asFile.readBytes())
        assertFalse((task.library.get().asFile.toPath() / "stale").exists())
    }

    @Test
    fun refusesPrebuiltRuntimeForFaultInjection() = withProject { project ->
        val task = project.tasks.create("runtime", BuildIosLibrary::class.java)
        task.prebuilt.set(project.layout.projectDirectory.file("librocky_uniffi.a"))
        task.qualification.set(true)
        assertFailsWith<IllegalStateException> { task.build() }
    }

    @OptIn(ExperimentalPathApi::class)
    private fun withProject(test: (org.gradle.api.Project) -> Unit) {
        val directory = Files.createTempDirectory("sdk-prebuilt-test")
        try {
            test(ProjectBuilder.builder().withProjectDir(directory.toFile()).build())
        } finally {
            directory.deleteRecursively()
        }
    }
}
