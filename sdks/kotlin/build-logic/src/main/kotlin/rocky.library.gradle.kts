import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// A published SDK library: rocky.multiplatform plus Android, as rocky:<project name>.
plugins {
    id("rocky.multiplatform")
    id("com.android.kotlin.multiplatform.library")
    id("rocky.publishing")
}

kotlin {
    android {
        namespace = "rocky." + project.name.removePrefix("rocky-").replace('-', '.')
        compileSdk = Android.COMPILE_SDK
        minSdk = Android.MIN_SDK
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
        }
    }
}
