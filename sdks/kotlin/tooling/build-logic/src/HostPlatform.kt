package rocky.build

/** The JNA resource prefix and native library name of the host platform. */
data class HostPlatform(val jnaPrefix: String, val library: String, val executableSuffix: String)

fun hostPlatform(): HostPlatform {
    val os = System.getProperty("os.name").lowercase()
    val arch = when (System.getProperty("os.arch")) {
        "aarch64", "arm64" -> "aarch64"
        "amd64", "x86_64" -> "x86-64"
        else -> error("Unsupported native SDK build architecture ${System.getProperty("os.arch")}")
    }
    return when {
        os.startsWith("mac") -> HostPlatform("darwin-$arch", "librocky_uniffi.dylib", "")
        os.startsWith("linux") -> HostPlatform("linux-$arch", "librocky_uniffi.so", "")
        os.startsWith("windows") -> HostPlatform("win32-$arch", "rocky_uniffi.dll", ".exe")
        else -> error("Unsupported native SDK build platform $os")
    }
}
