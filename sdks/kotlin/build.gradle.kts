// Puts the convention plugins' Kotlin and Android Gradle plugins on every project's classpath.
plugins {
    id("rocky.publishing") apply false
}

val libraries = listOf(":rocky-core", ":rocky-sqlite", ":rocky-room")
val core = project(":rocky-core")
val rockyVersion = providers.gradleProperty("rockyVersion").getOrElse("0.1.0")

val packageIos = tasks.register<PackageIos>("packageIos") {
    description = "Packages the iOS SDK with its XCFramework and inventory under build/packaged-ios."
    for (library in libraries) {
        for (publication in listOf("KotlinMultiplatform", "IosArm64", "IosSimulatorArm64")) {
            dependsOn("$library:publish${publication}PublicationToStagingRepository")
        }
    }
    repository.set(layout.buildDirectory.dir("release-maven"))
    deviceLibrary.set(core.layout.buildDirectory.dir("runtime/aarch64-apple-ios"))
    simulatorLibrary.set(core.layout.buildDirectory.dir("runtime/aarch64-apple-ios-sim"))
    headers.set(core.layout.buildDirectory.dir("generated/bindings/headers"))
    dependsOn(":rocky-core:buildRuntimeIosArm64", ":rocky-core:buildRuntimeIosSimulatorArm64", ":rocky-core:generateBindings")
    version.set(rockyVersion)
    kotlinVersion.set(libs.versions.kotlin)
    output.set(layout.buildDirectory.dir("packaged-ios"))
}

tasks.register<QualifyIosPackage>("qualifyIosPackage") {
    description = "Builds and tests an independent consumer of build/packaged-ios without Rust tools."
    packaged.set(packageIos.flatMap { it.output })
    consumer.set(layout.projectDirectory.dir("qualification/packaged-ios"))
    gradlew.set(layout.projectDirectory.file("gradlew"))
    entitlements.set(repositoryRoot.file("sdks/native/qualification/keychain-entitlements.plist"))
    version.set(rockyVersion)
    javaHome.set(providers.systemProperty("java.home"))
    developerDirectory.set(providers.environmentVariable("DEVELOPER_DIR"))
}
