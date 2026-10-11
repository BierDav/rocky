// A published Android-only library, as rocky:<project name>.
plugins {
    id("com.android.library")
    id("rocky.publishing")
}

android {
    namespace = "rocky." + project.name.removePrefix("rocky-").replace('-', '.')
    compileSdk = Android.COMPILE_SDK
    defaultConfig { minSdk = Android.MIN_SDK }
    publishing { singleVariant("release") }
}

publishing {
    publications {
        register<MavenPublication>("release") {
            afterEvaluate { from(components["release"]) }
        }
    }
}
