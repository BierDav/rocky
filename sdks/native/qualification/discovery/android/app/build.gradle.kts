plugins { id("com.android.application") version "9.3.1" }
android {
    namespace = "rocky.check.dnssd"
    compileSdk = 35
    defaultConfig {
        applicationId = "rocky.check.dnssd"
        minSdk = 26
        targetSdk = 35
        ndk { abiFilters += "arm64-v8a" }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
dependencies {
    implementation("net.java.dev.jna:jna:5.18.1@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
}
