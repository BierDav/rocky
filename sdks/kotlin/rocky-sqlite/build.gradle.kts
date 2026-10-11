import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi

// An androidx.sqlite driver on Rocky's own file, without a Room dependency.
plugins {
    id("rocky.library")
}

kotlin {
    // hostTest: the tests shared by the JVM and iOS, against the bundled reference engine.
    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    applyDefaultHierarchyTemplate {
        common {
            group("host") {
                withJvm()
                group("ios")
            }
        }
    }

    sourceSets {
        commonMain.dependencies {
            api(project(":rocky-core"))
            api(libs.sqlite)
        }
        named("hostTest").dependencies {
            implementation(kotlin("test"))
            implementation(project(":qualification:platform-test"))
            implementation(libs.sqlite.bundled)
        }
    }
}
