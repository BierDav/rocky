import java.io.File
import java.nio.file.Path
import java.util.zip.ZipFile
import kotlin.io.path.*

/** Building Rocky's runtime for Android; see docs/runtime/android.md. */
object Android {
    const val MIN_SDK = 26
    const val COMPILE_SDK = 35
    const val NDK_VERSION = "28.2.13676358"

    /** A packaged ABI: its Rust target and the NDK clang prefix. */
    class Abi(val name: String, val rustTarget: String, val clang: String)

    val ABIS = listOf(
        Abi("arm64-v8a", "aarch64-linux-android", "aarch64-linux-android"),
        Abi("x86_64", "x86_64-linux-android", "x86_64-linux-android"),
    )

    private const val RUSTUP_VERSION = "1.29.1"
    private val RUSTUP_SHA256 = mapOf(
        "aarch64-apple-darwin" to "ec1b9233e7f72990ecd8e62063fa7f6c3dfc2bec8e97f88bff165f9100ac696a",
        "x86_64-apple-darwin" to "259e2b84274434085163fe8d556510571772cda2aa6d87ca6aa664f57bc644e3",
        "x86_64-unknown-linux-gnu" to "dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71",
        "aarch64-unknown-linux-gnu" to "15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433",
    )
    private const val JNA_AAR = "https://repo1.maven.org/maven2/net/java/dev/jna/jna/5.18.1/jna-5.18.1.aar"
    private const val JNA_AAR_SHA256 = "7f053e3ec99e14dd71259c82c1c8a02738d64a13c31226b2acc170f3060951e0"

    /** The SDK of `ANDROID_HOME`, else Android Studio's default location. */
    fun sdkHome(): Path {
        for (name in listOf("ANDROID_HOME", "ANDROID_SDK_ROOT")) System.getenv(name)?.takeIf(String::isNotEmpty)?.let { return Path(it) }
        val mac = System.getProperty("os.name").lowercase().startsWith("mac")
        val default = Path(System.getProperty("user.home"), if (mac) "Library/Android/sdk" else "Android/Sdk")
        check(default.isDirectory()) { "Set ANDROID_HOME to an Android SDK with platform android-$COMPILE_SDK and NDK $NDK_VERSION" }
        return default
    }

    /**
     * Builds the stripped release runtime for every ABI into `[output]/<abi>/librocky_uniffi.so`,
     * linked by the pinned NDK for [MIN_SDK].
     */
    fun buildRuntime(repository: Path, cache: Path, output: Path) {
        val ndk = System.getenv("ANDROID_NDK_HOME")?.let(::Path) ?: (sdkHome() / "ndk" / NDK_VERSION)
        val bin = (ndk / "toolchains/llvm/prebuilt").takeIf { it.isDirectory() }?.listDirectoryEntries()?.firstOrNull()?.resolve("bin")
            ?: error("NDK $NDK_VERSION not found at $ndk; install it with sdkmanager \"ndk;$NDK_VERSION\"")
        val toolchain = Regex("""(?m)^channel = "([^"]+)"""").find((repository / "rust-toolchain.toml").readText())
            ?.groupValues?.get(1) ?: error("rust-toolchain.toml names no channel")
        val (rustup, rustEnvironment) = rustup(repository, cache, toolchain)
        val environment = rustEnvironment.toMutableMap()
        recreate(output)
        for (abi in ABIS) {
            val compiler = (bin / "${abi.clang}$MIN_SDK-clang").pathString
            val target = abi.rustTarget.replace('-', '_')
            environment["CARGO_TARGET_${target.uppercase()}_LINKER"] = compiler
            environment["CC_$target"] = compiler
            environment["AR_$target"] = (bin / "llvm-ar").pathString
            run(
                listOf(
                    rustup, "run", toolchain, "cargo", "build", "--manifest-path", "sdks/uniffi/Cargo.toml",
                    "--locked", "--release", "--target", abi.rustTarget,
                ),
                repository,
                environment,
            )
            val built = repository / "sdks/uniffi/target" / abi.rustTarget / "release/librocky_uniffi.so"
            val stripped = (output / abi.name / "librocky_uniffi.so").createParentDirectories()
            run(listOf((bin / "llvm-strip").pathString, "--strip-unneeded", "-o", stripped.pathString, built.pathString), repository)
        }
        extractJnaDispatch(cache, output)
    }

    /** Copies JNA's `libjnidispatch.so` for every ABI from its checksum-pinned AAR into `[output]/<abi>/`. */
    private fun extractJnaDispatch(cache: Path, output: Path) {
        val aar = fetch(JNA_AAR, cache / "jna-5.18.1.aar", JNA_AAR_SHA256)
        ZipFile(aar.toFile()).use { zip ->
            for (abi in ABIS) {
                val entry = zip.getEntry("jni/${abi.name}/libjnidispatch.so") ?: error("JNA has no ${abi.name} dispatch library")
                zip.getInputStream(entry).use { input ->
                    (output / abi.name / "libjnidispatch.so").createParentDirectories().outputStream().use(input::copyTo)
                }
            }
        }
    }

    /** A rustup with the Android targets and its environment: the one on `PATH`, or one provisioned under [cache]. */
    private fun rustup(repository: Path, cache: Path, toolchain: String): Pair<String, Map<String, String>> {
        val environment = mutableMapOf<String, String>()
        val rustup = onPath("rustup") ?: run {
            val home = (cache / "rust").createDirectories()
            environment["RUSTUP_HOME"] = (home / "rustup").pathString
            environment["CARGO_HOME"] = (home / "cargo").pathString
            environment["PATH"] = (home / "cargo/bin").pathString + File.pathSeparator + System.getenv("PATH")
            if (!(home / "cargo/bin/rustup").exists()) {
                val host = rustupHost()
                val init = fetch(
                    "https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/$host/rustup-init",
                    home / "rustup-init",
                    RUSTUP_SHA256.getValue(host),
                )
                init.toFile().setExecutable(true)
                run(
                    listOf(init.pathString, "-y", "--no-modify-path", "--profile", "minimal", "--default-toolchain", "none"),
                    repository,
                    environment,
                )
            }
            (home / "cargo/bin/rustup").pathString
        }
        // The compiler rust-toolchain.toml pins, named explicitly so no other cargo on PATH is used.
        run(listOf(rustup, "toolchain", "install", toolchain, "--profile", "minimal"), repository, environment)
        run(listOf(rustup, "target", "add", "--toolchain", toolchain) + ABIS.map { it.rustTarget }, repository, environment)
        return rustup to environment
    }

    private fun rustupHost(): String {
        val os = System.getProperty("os.name").lowercase()
        val arch = when (System.getProperty("os.arch")) {
            "aarch64", "arm64" -> "aarch64"
            "amd64", "x86_64" -> "x86_64"
            else -> null
        }
        val system = when {
            os.startsWith("mac") -> "apple-darwin"
            os.startsWith("linux") -> "unknown-linux-gnu"
            else -> null
        }
        return "$arch-$system".takeIf { arch != null && system != null && it in RUSTUP_SHA256 }
            ?: error("Install rustup; no pinned rustup-init for this host")
    }
}
