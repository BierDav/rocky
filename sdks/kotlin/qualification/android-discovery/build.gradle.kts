// The Android check of local-network discovery: one database with the mDNS path on, followed by
// RockyLifecycle; see docs/runtime/discovery.md. Run it with :qualification:android-discovery:qualify.
plugins {
    id("rocky.android-app")
}

android {
    namespace = "rocky.check.discovery"
    defaultConfig { applicationId = "rocky.check.discovery" }
}

dependencies {
    implementation(project(":rocky-core"))
}

tasks.register<AndroidDiscoveryCheck>("qualify") {
    description = "Checks the multicast lock on an emulator; -Procky.android.avd boots one."
    apks.set(layout.buildDirectory.dir("outputs/apk/debug"))
    dependsOn("assembleDebug")
    avd.set(providers.gradleProperty("rocky.android.avd"))
}
