import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.*
import java.io.File
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/**
 * Installs the discovery check app on an emulator: while local discovery is on, RockyLifecycle holds
 * the Wi-Fi multicast lock in the foreground and releases it in the background. Uses a booted
 * emulator, or boots the AVD [avd] names.
 */
@UntrackedTask(because = "Drives a device on every run")
abstract class AndroidDiscoveryCheck : DefaultTask() {
    @get:InputDirectory abstract val apks: DirectoryProperty

    @get:Input @get:Optional
    abstract val avd: Property<String>

    private val sdk = Android.sdkHome().toFile()
    private val adb = File(sdk, "platform-tools/adb").path

    @TaskAction
    fun check() {
        val apk = apks.get().asFile.walk().firstOrNull { it.extension == "apk" } ?: error("no check APK in ${apks.get()}")
        var device = emulator()
        var started: Process? = null
        if (device == null) {
            val name = avd.orNull ?: error("boot an emulator or name one with -Procky.android.avd")
            started = ProcessBuilder(File(sdk, "emulator/emulator").path, "-avd", name, "-no-window", "-no-audio", "-no-snapshot-save")
                .redirectOutput(ProcessBuilder.Redirect.DISCARD).redirectErrorStream(true).start()
            repeat(120) {
                device = device ?: emulator()
                if (device != null && shell(device!!, "getprop", "sys.boot_completed").trim() == "1") return@repeat
                Thread.sleep(2_000)
            }
        }
        val serial = device ?: error("no emulator came up")
        try {
            adb(serial, "install", "-r", apk.path)
            adb(serial, "logcat", "-c")
            shell(serial, "am", "start", "-W", "-n", "$PACKAGE/.CheckActivity")
            await(serial, "links background=closed foreground=open")
            await(serial, "foreground=true lock=true local=On")
            check(held(serial) > 0) { "the system does not list the multicast lock" }
            adb(serial, "logcat", "-c")
            shell(serial, "input", "keyevent", "KEYCODE_HOME")
            await(serial, "foreground=false lock=false local=Off")
            check(held(serial) == 0) { "the multicast lock is still held in the background" }
            adb(serial, "logcat", "-c")
            shell(serial, "am", "start", "-W", "-n", "$PACKAGE/.CheckActivity")
            await(serial, "foreground=true lock=true local=On")
            logger.lifecycle("Android discovery: the multicast lock is held in the foreground and released in the background")
        } finally {
            runCatching { adb(serial, "uninstall", PACKAGE) }
            if (started != null) {
                runCatching { adb(serial, "emu", "kill") }
                started.waitFor(30, TimeUnit.SECONDS)
            }
        }
    }

    private fun emulator(): String? = capture(adb, "devices").lines().map { it.split(Regex("\\s+")) }
        .firstOrNull { it.size >= 2 && it[0].startsWith("emulator-") && it[1] == "device" }?.get(0)

    /** Waits for the line the app logs once a lifecycle transition was applied. */
    private fun await(serial: String, state: String) {
        val process = ProcessBuilder(adb, "-s", serial, "logcat", "-s", "$TAG:*").redirectErrorStream(true).start()
        try {
            val line = CompletableFuture.supplyAsync {
                process.inputStream.bufferedReader().useLines { lines ->
                    lines.firstOrNull { state in it || "failed " in it } ?: error("logcat ended before $state")
                }
            }.get(60, TimeUnit.SECONDS)
            check(state in line) { line }
            logger.lifecycle(line)
        } finally {
            process.destroyForcibly()
        }
    }

    private fun held(serial: String) = shell(serial, "dumpsys", "wifi").lines().count { "Multicaster{rocky-discovery" in it }

    private fun shell(serial: String, vararg command: String) = capture(adb, "-s", serial, "shell", *command)

    private fun adb(serial: String, vararg command: String) = capture(adb, "-s", serial, *command)

    private fun capture(vararg command: String): String {
        val process = ProcessBuilder(*command).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        check(process.waitFor() == 0) { "${command.joinToString(" ")} failed:\n$output" }
        return output
    }

    private companion object {
        const val PACKAGE = "rocky.check.discovery"
        const val TAG = "RockyDiscoveryCheck"
    }
}
