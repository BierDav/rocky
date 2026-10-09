import rocky.build.Android
import rocky.build.RUST_SOURCES
import rocky.build.RuntimeArtifact
import rocky.build.rustSourcesDigest

// Rocky's Android runtime as an AAR, which the Kotlin toolchain cannot build itself; see README.md.
plugins {
    id("com.android.library") version "9.3.1"
    `maven-publish`
}

val repository = rootDir.resolve("../../..").canonicalFile

/** Builds librocky_uniffi.so and places JNA's libjnidispatch.so for every packaged ABI. */
abstract class RockyRuntime : DefaultTask() {
    @get:Internal abstract val repository: DirectoryProperty

    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sources: ConfigurableFileCollection

    @get:OutputDirectory abstract val libraries: DirectoryProperty

    @TaskAction
    fun build() {
        val root = repository.get().asFile.toPath()
        val output = libraries.get().asFile.toPath()
        Android.buildRuntime(root, output)
        Android.extractJnaDispatch(root.resolve("local/android-sdk-build").also { it.toFile().mkdirs() }, output)
    }
}

val runtime = tasks.register<RockyRuntime>("rockyRuntime") {
    repository.set(rootDir.resolve("../../.."))
    sources.from(RUST_SOURCES.map { File(rootDir, "../../../$it") })
}

android {
    namespace = "rocky.runtime"
    compileSdk = 35
    defaultConfig { minSdk = Android.MIN_SDK }
    publishing { singleVariant("release") }
}

androidComponents {
    onVariants { variant -> variant.sources.jniLibs?.addGeneratedSourceDirectory(runtime, RockyRuntime::libraries) }
}

publishing {
    publications {
        register<MavenPublication>("release") {
            groupId = RuntimeArtifact.GROUP
            artifactId = RuntimeArtifact.NAME
            // Releases pass -ProckyVersion; Maven local keeps the development version the toolchain verifies.
            version = providers.gradleProperty("rockyVersion").getOrElse(RuntimeArtifact.VERSION)
            afterEvaluate { from(components["release"]) }
            pom.properties.put(RuntimeArtifact.DIGEST_PROPERTY, providers.provider { rustSourcesDigest(repository.toPath()) })
        }
    }
    // The release workflow uploads this directory to GitHub Packages; see docs/releasing.md.
    repositories {
        maven {
            name = "staging"
            url = uri(repository.resolve("local/release-maven"))
        }
    }
}
