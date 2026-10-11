package org.example.examples.shopping

import kotlinx.datetime.TimeZone
import org.example.examples.shopping.util.formatDate
import org.example.examples.shopping.util.hex
import org.example.examples.shopping.util.randomBytes
import org.example.examples.shopping.util.sha256
import kotlin.test.*

class PlatformTest {
    @Test fun sha256MatchesPublishedVectors() {
        assertEquals("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", sha256(byteArrayOf()).hex())
        assertEquals("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", sha256("abc".encodeToByteArray()).hex())
    }

    @Test fun datesUseTheSelectedTimeZoneAndDaylightSavingRules() {
        assertEquals("1970-01-01 00:00", formatDate(0, TimeZone.UTC))
        assertEquals("1969-12-31 19:00", formatDate(0, TimeZone.of("America/New_York")))
        assertEquals("2024-03-31 01:59", formatDate(1711846740, TimeZone.of("Europe/Vienna")))
        assertEquals("2024-03-31 03:00", formatDate(1711846800, TimeZone.of("Europe/Vienna")))
    }

    @Test fun randomBytesSupportsEmptyAndRequestedSizes() {
        assertContentEquals(byteArrayOf(), randomBytes(0))
        assertEquals(32, randomBytes(32).size)
        assertFails { randomBytes(-1) }
    }
}
