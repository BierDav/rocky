// The artifact-only conformance consumer of the packaged JVM SDK.
plugins {
    id("rocky.jvm")
}

dependencies {
    implementation(project(":rocky-core"))
}

kotlin {
    compilerOptions { allWarningsAsErrors.set(true) }
}

val consumerMain = "consumer.ConsumerKt"

val packageSdk = tasks.register<PackageJvmSdk>("packageSdk") {
    description = "Packages the JVM SDK jars with a consumer compiled against them."
    libraries.from(configurations.runtimeClasspath)
    consumer.set(tasks.jar.flatMap { it.archiveFile })
    consumerSource.set(layout.projectDirectory.file("src/main/kotlin/consumer/Consumer.kt"))
    mainClass.set(consumerMain)
    output.set(layout.buildDirectory.dir("packaged-sdk"))
}

val qualifyPackage = tasks.register<QualifyJvmPackage>("qualifyPackage") {
    description = "Runs the packaged consumer from a clean copy with only a JVM."
    packaged.set(packageSdk.flatMap { it.output })
    mainClass.set(consumerMain)
    java.set(providers.systemProperty("java.home").map { "$it/bin/java" })
}

tasks.check { dependsOn(qualifyPackage) }
