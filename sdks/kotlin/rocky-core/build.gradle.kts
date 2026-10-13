import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType
import org.jetbrains.kotlin.gradle.plugin.mpp.KotlinNativeTarget
import org.jetbrains.kotlin.gradle.plugin.mpp.TestExecutable
import org.jetbrains.kotlin.gradle.targets.native.tasks.KotlinNativeSimulatorTest
import org.jetbrains.kotlin.konan.target.HostManager

// One Kotlin API over the native runtime: JNA on the JVM and Android, cinterop on iOS.
plugins {
    id("rocky.library")
}

val bindings = tasks.register<GenerateBindings>("generateBindings") {
    repository.set(repositoryRoot)
    sources.from(rustSources())
    val output = layout.buildDirectory.dir("generated/bindings")
    common.set(output.map { it.dir("common") })
    jna.set(output.map { it.dir("jna") })
    ios.set(output.map { it.dir("ios") })
    headers.set(output.map { it.dir("headers") })
    hostLibrary.set(output.map { it.dir("host") })
}

// IDE imports need the same native declarations as compilation, including after a clean checkout.
tasks.named("prepareKotlinIdeaImport") {
    dependsOn(bindings)
}

// The in-repository simulator tests need the runtime's fault injection; published builds never have it.
val faultInjection = providers.gradleProperty("rocky.qualification").map(String::toBoolean).orElse(false)

kotlin {
    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    applyDefaultHierarchyTemplate {
        common {
            group("jna") {
                withJvm()
                withCompilations { it.platformType == KotlinPlatformType.androidJvm }
            }
        }
    }

    targets.withType<KotlinNativeTarget>().configureEach {
        val rustTarget = if (name == "iosSimulatorArm64") "aarch64-apple-ios-sim" else "aarch64-apple-ios"
        val library = tasks.register<BuildIosLibrary>("buildRuntime${name.replaceFirstChar(Char::uppercase)}") {
            repository.set(repositoryRoot)
            sources.from(rustSources())
            target.set(rustTarget)
            qualification.set(faultInjection.map { it && rustTarget.endsWith("-sim") })
            library.set(layout.buildDirectory.dir("runtime/$rustTarget"))
        }
        compilations.getByName("main").cinterops.create("rocky") {
            definitionFile.set(layout.projectDirectory.file("src/nativeInterop/cinterop/rocky.def"))
            includeDirs(bindings.flatMap { it.headers })
            extraOpts("-libraryPath", library.get().library.get().asFile.path)
        }
        tasks.named(compilations.getByName("main").cinterops.getByName("rocky").interopProcessingTaskName) {
            inputs.dir(bindings.flatMap { it.headers })
            // Disabled native targets must not pull a Rust cross-build into host metadata publication.
            if (HostManager().isEnabled(konanTarget)) inputs.dir(library.flatMap { it.library })
        }
        if (name == "iosSimulatorArm64") {
            // Standalone simulator executables need an app identity for Keychain access.
            binaries.withType<TestExecutable>().configureEach {
                linkerOpts("-sectcreate", "__TEXT", "__entitlements", repositoryRoot.file("sdks/native/qualification/keychain-entitlements.plist").asFile.path)
            }
        }
    }

    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    sourceSets {
        commonMain {
            generatedKotlin.srcDir(bindings.flatMap { it.common })
            dependencies {
                api(libs.kotlinx.coroutines.core)
                api(libs.kotlinx.serialization.json)
            }
        }
        named("jnaMain") {
            generatedKotlin.srcDir(bindings.flatMap { it.jna })
            dependencies {
                implementation(libs.jna)
            }
        }
        jvmMain {
            resources.srcDir(bindings.flatMap { it.hostLibrary })
            // Releases add the other hosts' libraries, built by their own runners, to the one jar.
            providers.gradleProperty("rocky.jvmNatives").orNull?.let { resources.srcDir(it) }
        }
        androidMain {
            dependencies {
                // The native libraries for every packaged ABI.
                implementation(project(":rocky-runtime-android"))
            }
        }
        iosMain {
            generatedKotlin.srcDir(bindings.flatMap { it.ios })
            dependencies {
                implementation(libs.atomicfu)
            }
        }
        commonTest {
            dependencies {
                implementation(kotlin("test"))
            }
        }
    }

    android {
        // Keeps the classes JNA reaches by reflection.
        optimization.consumerKeepRules.apply {
            publish = true
            file("proguard-android.txt")
        }
    }
}

tasks.withType<KotlinNativeSimulatorTest>().configureEach {
    // Reclaiming sockets needs the runtime's fault injection.
    if (!faultInjection.get()) filter.excludeTestsMatching("rocky.IosResumeTest")
}
