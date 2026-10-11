import org.jetbrains.kotlin.gradle.plugin.mpp.TestExecutable
import org.jetbrains.kotlin.gradle.targets.native.tasks.KotlinNativeSimulatorTest

// An independent consumer of the packaged iOS SDK. Run by :packageIos's qualifyIosPackage
// with Cargo and rustup absent from PATH.
plugins {
    alias(libs.plugins.kotlin.multiplatform)
}

val sdkVersion = providers.gradleProperty("rockyVersion").getOrElse("0.1.0")
val entitlements = providers.gradleProperty("rocky.entitlements")

kotlin {
    iosArm64()
    iosSimulatorArm64 {
        // Standalone simulator executables need an app identity for Keychain access.
        binaries.withType<TestExecutable>().configureEach {
            linkerOpts("-sectcreate", "__TEXT", "__entitlements", entitlements.get())
        }
    }
    targets.withType<org.jetbrains.kotlin.gradle.plugin.mpp.KotlinNativeTarget>().configureEach {
        binaries.withType<TestExecutable>().configureEach {
            freeCompilerArgs += "-Xbinary=pagedAllocator=false"
        }
    }
    sourceSets {
        iosMain.dependencies {
            implementation("rocky:rocky-core:$sdkVersion")
            implementation("rocky:rocky-sqlite:$sdkVersion")
            implementation("rocky:rocky-room:$sdkVersion")
        }
        iosTest.dependencies {
            implementation(kotlin("test"))
        }
    }
}

tasks.withType<KotlinNativeSimulatorTest>().configureEach {
    // The Keychain exists only on a booted simulator, not in a standalone spawn.
    standalone.set(false)
    val simulator = device
    doFirst {
        val boot = ProcessBuilder("xcrun", "simctl", "bootstatus", simulator.get(), "-b").inheritIO().start()
        check(boot.waitFor() == 0) { "cannot boot the simulator ${simulator.get()}" }
    }
}
