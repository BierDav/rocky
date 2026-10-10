package rocky.build

import org.jetbrains.amper.plugins.*
import java.nio.file.Path
import java.util.zip.ZipEntry
import java.util.zip.ZipFile
import java.util.zip.ZipOutputStream
import kotlin.io.path.*

// Releases set ROCKY_VERSION (docs/releasing.md); it is not an input, so rebuild after changing it.
private val VERSION = System.getenv("ROCKY_VERSION") ?: "0.1.0"
private const val KOTLIN = "2.4.20"
private const val GOOGLE = "https://dl.google.com/android/maven2/"

// Compile-only inputs of rocky-sqlite and rocky-room; consumers resolve Room themselves.
private val ROOM = listOf(
    "androidx/room/room-runtime-android/2.8.4/room-runtime-android-2.8.4.aar" to "3293575082024fa0a92b081c4e2d0609f14b5c109635ec227bc509db141a4e20",
    "androidx/room/room-common-jvm/2.8.4/room-common-jvm-2.8.4.jar" to "dc7b4e1a75a790f29c97afd5516b8bf8002d9b6e83e295bc569b33566f3b1ec0",
    "androidx/sqlite/sqlite-android/2.6.2/sqlite-android-2.6.2.aar" to "514f8de4f7bdba2fa8d748d2b22fdcdfb623bd3770a667bf0adc0ad259f03ed2",
    "androidx/annotation/annotation-jvm/1.9.1/annotation-jvm-1.9.1.jar" to "1e343917ebf27ba96fe4dc52b1cad7fd32b738fbc6355bb6cd5b3b305d7212d0",
)

/**
 * Fails unless Maven local holds the runtime AAR built from the current Rust sources, naming the Gradle
 * command that publishes it. Its output directory stays empty; contributing it makes every build verify.
 */
@TaskAction
fun verifyRuntime(repositoryRoot: String, @Input rustSources: List<Path>, @Output verified: Path) {
    val pom = RuntimeArtifact.directory() / "${RuntimeArtifact.NAME}-${RuntimeArtifact.VERSION}.pom"
    val publish = "sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal"
    check(pom.exists()) { "Rocky's Android runtime is not in Maven local; run $publish" }
    val published = Regex("<${Regex.escape(RuntimeArtifact.DIGEST_PROPERTY)}>([0-9a-f]+)<").find(pom.readText())?.groupValues?.get(1)
    check(published == rustSourcesDigest(Path(repositoryRoot))) {
        "Rocky's Android runtime in Maven local was built from other Rust sources; run $publish"
    }
    recreate(verified)
}

/**
 * The Android SDK as a local Maven repository with a SHA-256 inventory: `rocky-core-android` (an AAR with
 * the published runtime for each ABI), `rocky-sqlite` and `rocky-room`; see docs/runtime/android.md.
 */
@TaskAction
fun packageAndroid(
    repositoryRoot: String,
    @Input sources: ModuleSources,
    @Input androidSources: Path,
    @Input keepRules: Path,
    @Input sqliteSources: Path,
    @Input roomSources: Path,
    @Input verified: Path,
    @Input dependencies: Classpath,
    @Output output: Path,
) {
    val cache = (Path(repositoryRoot) / "local/android-sdk-build").createDirectories()
    val work = recreate(cache / "work")
    val repository = recreate(output) / "maven"
    val base = listOf(Android.platformJar()) + dependencies.resolvedFiles
    val core = work / "classes.jar"
    val coreSources = (sources.sourceDirectories + listOf(androidSources)).filter {
        it.exists() && it.name !in setOf("ios", "src@ios")
    }.distinct()
    val common = coreSources.filter { it.name == "src" || it.name == "common" }
    compile(coreSources, base, core, common)
    val aar = work / "rocky-core-android.aar"
    ZipOutputStream(aar.outputStream()).use { archive ->
        archive.put("classes.jar", core.readBytes())
        archive.put(
            "AndroidManifest.xml",
            """
            |<?xml version="1.0" encoding="utf-8"?>
            |<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="rocky.android">
            |    <uses-sdk android:minSdkVersion="${Android.MIN_SDK}"/>
            |    <!-- Local-network discovery holds the Wi-Fi multicast lock while it runs. -->
            |    <uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE"/>
            |</manifest>
            |
            """.trimMargin().toByteArray(),
        )
        archive.put("R.txt", ByteArray(0))
        archive.put("proguard.txt", keepRules.readBytes())
        val runtime = RuntimeArtifact.directory() / "${RuntimeArtifact.NAME}-${RuntimeArtifact.VERSION}.aar"
        ZipFile(runtime.toFile()).use { published ->
            for (abi in Android.ABIS) {
                val name = "jni/${abi.name}/librocky_uniffi.so"
                archive.put(name, published.getInputStream(published.getEntry(name) ?: error("$runtime has no $name")).readBytes())
            }
        }
    }
    publish(
        repository,
        "rocky-core-android",
        "aar",
        aar,
        listOf(
            Pom("org.jetbrains.kotlin", "kotlin-stdlib", KOTLIN),
            Pom("org.jetbrains.kotlinx", "kotlinx-coroutines-android", "1.10.2"),
            Pom("org.jetbrains.kotlinx", "kotlinx-serialization-json", "1.9.0"),
            Pom("net.java.dev.jna", "jna", "5.18.1", "aar"),
        ),
    )
    val room = ROOM.map { (path, digest) ->
        val downloaded = fetch(GOOGLE + path, cache / path.substringAfterLast('/'), digest)
        if (downloaded.extension != "aar") {
            downloaded
        } else {
            (cache / (downloaded.nameWithoutExtension + ".jar")).also { classes ->
                ZipFile(downloaded.toFile()).use { classes.writeBytes(it.getInputStream(it.getEntry("classes.jar")).readBytes()) }
            }
        }
    }
    val sqlite = work / "rocky-sqlite.jar"
    compile(listOf(sqliteSources), base + room.filter { it.name.startsWith("sqlite-android") } + listOf(core), sqlite)
    publish(
        repository,
        "rocky-sqlite",
        "jar",
        sqlite,
        listOf(
            Pom("rocky", "rocky-core-android", VERSION, "aar"),
            Pom("androidx.sqlite", "sqlite", "2.6.2"),
        ),
    )
    val roomJar = work / "rocky-room.jar"
    compile(listOf(roomSources), base + room + listOf(core, sqlite), roomJar)
    publish(
        repository,
        "rocky-room",
        "jar",
        roomJar,
        listOf(
            Pom("rocky", "rocky-core-android", VERSION, "aar"),
            Pom("rocky", "rocky-sqlite", VERSION),
            Pom("androidx.room", "room-runtime", "2.8.4"),
        ),
    )
    writeManifest(output)
    println(output)
}

@OptIn(ExperimentalPathApi::class)
private fun compile(sources: List<Path>, classpath: List<Path>, destination: Path, common: List<Path> = emptyList()) {
    fun files(directories: List<Path>) = directories.flatMap { directory ->
        directory.walk().filter { it.extension == "kt" }.map { it.toAbsolutePath().normalize() }.toList()
    }
    val commonFiles = files(common)
    val multiplatform = if (commonFiles.isEmpty()) {
        emptyList()
    } else {
        listOf(
            "-Xmulti-platform",
            "-Xexpect-actual-classes",
            "-Xcommon-sources=" + commonFiles.joinToString(",") { it.pathString },
        )
    }
    val compilation = compileKotlin(files(sources), classpath, destination, listOf("-Werror", "-no-jdk") + multiplatform)
    print(compilation.messages)
    check(compilation.succeeded) { "compiling $destination failed" }
}

private fun ZipOutputStream.put(name: String, bytes: ByteArray) {
    putNextEntry(ZipEntry(name))
    write(bytes)
    closeEntry()
}

private class Pom(val group: String, val artifact: String, val version: String, val type: String? = null)

private fun publish(repository: Path, artifact: String, packaging: String, binary: Path, dependencies: List<Pom>) {
    val directory = (repository / "rocky" / artifact / VERSION).createDirectories()
    binary.copyTo(directory / "$artifact-$VERSION.$packaging")
    val items = dependencies.joinToString("") {
        "<dependency><groupId>${it.group}</groupId><artifactId>${it.artifact}</artifactId><version>${it.version}</version>" +
            (it.type?.let { type -> "<type>$type</type>" } ?: "") +
            "<scope>compile</scope></dependency>"
    }
    (directory / "$artifact-$VERSION.pom").writeText(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n" +
            "<project xmlns=\"http://maven.apache.org/POM/4.0.0\"><modelVersion>4.0.0</modelVersion><groupId>rocky</groupId>" +
            "<artifactId>$artifact</artifactId><version>$VERSION</version><packaging>$packaging</packaging>" +
            "<dependencies>$items</dependencies></project>\n",
    )
}
