// Rocky's Kotlin SDK; see README.md.
pluginManagement {
    includeBuild("build-logic")
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "rocky-kotlin"

// The published libraries.
include(":rocky-core", ":rocky-sqlite", ":rocky-room", ":rocky-runtime-android")

// Tests and conformance checks; never published.
include(
    ":qualification:platform-test",
    ":qualification:room",
    ":qualification:schema",
    ":qualification:inventory",
    ":qualification:packaged-jvm",
    ":qualification:android-discovery",
)

// Without ANDROID_HOME, use Android Studio's default SDK location.
val localProperties = settingsDir.resolve("local.properties")
if (System.getenv("ANDROID_HOME").isNullOrEmpty() && !localProperties.exists()) {
    val mac = System.getProperty("os.name").lowercase().startsWith("mac")
    val default = File(System.getProperty("user.home"), if (mac) "Library/Android/sdk" else "Android/Sdk")
    if (default.isDirectory) localProperties.writeText("sdk.dir=${default.path}\n")
}
