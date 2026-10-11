package org.example.examples.shopping

import kotlinx.coroutines.runBlocking
import java.nio.file.Files
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse

class ContactLabelsTest {
    @Test fun labelsSurviveRestartWithoutSavingNodeKeys() = runBlocking {
        val directory = Files.createTempDirectory("contact-labels").toFile()
        try {
            val file = directory.resolve("contact-labels.json")
            val node = "ab".repeat(32)
            ContactLabels.open(file.path).name(node, "Kitchen phone")
            assertFalse(file.readText().contains(node))
            assertEquals("Kitchen phone", ContactLabels.open(file.path).label(node))
            assertEquals(listOf("contact-labels.json"), directory.list()!!.toList())
        } finally {
            directory.deleteRecursively()
        }
    }
}
