package org.example.examples.shopping.diagnostics

import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class DiagnosticLogTest {
    @Test
    fun categorizesNativeTimeoutsWithoutPayloads() {
        assertTrue(failureCategory("rejected: native read timeout") == "native_read_timeout")
        assertTrue(failureCategory("link channel read timeout") == "link_channel_read_timeout")
        assertTrue(failureCategory(null) == "none")
        assertFalse(failureCategory("private-row-and-token").contains("private"))
        assertTrue(failureCategory("rejected: checkpoint signature mismatch: private-row-and-token") == "rejected,checkpoint,mismatch,signature")
    }

    @Test
    fun rotatesAndOmitsExceptionMessages() {
        val directory = Files.createTempDirectory("shopping-diagnostics").toFile()
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        try {
            val log = DiagnosticLog(directory, "test", "test")
            val file = directory.resolve("diagnostics.log")
            log.write("failure", IllegalStateException("private-row-and-token"))
            assertTrue(file.readText().contains("java.lang.IllegalStateException"))
            assertFalse(file.readText().contains("private-row-and-token"))
            repeat(5) {
                file.writeBytes(ByteArray(2 * 1024 * 1024))
                log.write("rotated")
            }
            assertTrue(file.readText().contains("rotated"))
            assertTrue(directory.resolve("diagnostics.log.3").isFile)
            assertFalse(directory.resolve("diagnostics.log.4").exists())
        } finally {
            Thread.setDefaultUncaughtExceptionHandler(previous)
            directory.deleteRecursively()
        }
    }
}
