import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.plugin.mpp.TestExecutable
import org.jetbrains.kotlin.gradle.targets.native.tasks.KotlinNativeSimulatorTest
import org.jetbrains.kotlin.gradle.targets.native.tasks.KotlinNativeTest

// Kotlin Multiplatform for the JVM and arm64 iOS; rocky.library adds Android and publishing.
plugins {
    id("org.jetbrains.kotlin.multiplatform")
}

kotlin {
    // Packaged JVM consumers run on Java 17.
    jvm {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
            freeCompilerArgs.add("-Xjdk-release=17")
        }
    }
    listOf(iosArm64(), iosSimulatorArm64()).forEach { target ->
        // Use per-object allocation when linking an uncached test runtime.
        target.binaries.withType<TestExecutable>().configureEach {
            freeCompilerArgs += "-Xbinary=pagedAllocator=false"
        }
    }
    compilerOptions {
        freeCompilerArgs.add("-Xexpect-actual-classes")
    }
}

tasks.withType<Test>().configureEach {
    useJUnitPlatform()
}

tasks.withType<KotlinNativeTest>().configureEach {
    // Where the tests find the repository's fixtures; simctl hands its children the prefixed name.
    environment("ROCKY_TEST_ROOT", repositoryRoot.asFile.path)
    environment("SIMCTL_CHILD_ROCKY_TEST_ROOT", repositoryRoot.asFile.path)
}

tasks.withType<KotlinNativeSimulatorTest>().configureEach {
    // The Keychain exists only on a booted simulator, not in a standalone spawn.
    standalone.set(false)
    val simulator = device
    doFirst { run(listOf("xcrun", "simctl", "bootstatus", simulator.get(), "-b"), temporaryDir.toPath()) }
}
