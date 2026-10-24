package org.example.examples.shopping.diagnostics

import java.io.File
import java.time.Instant

/** Four 2 MiB files; each record is closed before returning. */
class DiagnosticLog(private val directory: File, build: String, instance: String) {
    private val file = File(directory, "diagnostics.log")

    init {
        check(directory.mkdirs() || directory.isDirectory) { "Cannot create diagnostics directory" }
        write("startup build=$build instance=$instance runtime=replica35,row12,checkpoint17,link16")
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, error ->
            runCatching { write("uncaught thread=${thread.name}", error) }
            previous?.uncaughtException(thread, error)
        }
    }

    @Synchronized
    fun write(message: String, error: Throwable? = null) {
        try {
            val record = buildString {
                append("${Instant.now()} ${message.take(4096)}\n")
                // Exception messages can contain SQL values or pairing URIs. Keep types and frames only.
                var cause = error
                repeat(8) {
                    val current = cause ?: return@repeat
                    append("exception=${current.javaClass.name}\n")
                    current.stackTrace.take(32).forEach { append("  at $it\n") }
                    current.suppressed.take(4).forEach { append("suppressed=${it.javaClass.name}\n") }
                    cause = current.cause
                }
            }.take(16 * 1024).toByteArray(Charsets.UTF_8)
            if (file.length() + record.size > 2 * 1024 * 1024) {
                File(directory, "diagnostics.log.3").delete()
                for (index in 2 downTo 1) {
                    val source = File(directory, "diagnostics.log.$index")
                    if (source.exists()) check(source.renameTo(File(directory, "diagnostics.log.${index + 1}")))
                }
                check(file.renameTo(File(directory, "diagnostics.log.1")))
            }
            java.io.FileOutputStream(file, true).use { it.write(record) }
        } catch (failure: Exception) {
            System.err.println("Shopping diagnostics write failed: ${failure.javaClass.name}")
        }
    }
}
