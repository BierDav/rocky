pluginManagement {
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
rootProject.name = "rocky-runtime-android"

// Without ANDROID_HOME, use the Android Studio default SDK location, as the Kotlin toolchain plugins do.
val localProperties = settingsDir.resolve("local.properties")
if (System.getenv("ANDROID_HOME").isNullOrEmpty() && !localProperties.exists()) {
    val mac = System.getProperty("os.name").lowercase().startsWith("mac")
    val default = File(System.getProperty("user.home"), if (mac) "Library/Android/sdk" else "Android/Sdk")
    if (default.isDirectory) localProperties.writeText("sdk.dir=${default.path}\n")
}
