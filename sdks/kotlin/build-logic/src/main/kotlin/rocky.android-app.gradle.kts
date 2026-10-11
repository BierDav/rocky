import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// An Android application for on-device checks.
plugins {
    id("com.android.application")
}

android {
    compileSdk = Android.COMPILE_SDK
    defaultConfig {
        minSdk = Android.MIN_SDK
        targetSdk = Android.COMPILE_SDK
        // The ABIs Rocky's Android runtime is built for.
        ndk { abiFilters += Android.ABIS.map { it.name } }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
}
