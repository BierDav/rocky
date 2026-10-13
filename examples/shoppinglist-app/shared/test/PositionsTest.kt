package org.example.examples.shopping

import org.example.examples.shopping.data.Positions
import kotlin.test.*

class PositionsTest {
    @Test fun `keys fall strictly between their neighbours and never end in 0`() {
        val spread = Positions.spread(3)
        assertEquals(listOf("0000000005", "0000000015", "0000000025"), spread)
        val cases = listOf(null to null, null to spread[0], spread[0] to spread[1], spread[2] to null, "5" to "51", "1" to "2", "19" to "2")
        for ((after, before) in cases) {
            val key = Positions.between(after, before)
            assertTrue((after == null || after < key) && (before == null || key < before), "$after < $key < $before")
            assertFalse(key.endsWith('0'), key)
        }
    }

    @Test fun `repeated places at one spot keep fitting`() {
        var low: String? = null
        val high = Positions.spread(1).single()
        repeat(40) {
            val key = Positions.between(low, high)
            assertTrue(Positions.fits(low, key) && key < high)
            low = key
        }
    }

    @Test fun `equal keys from concurrent places leave no room`() {
        assertFalse(Positions.fits("0000000015", "0000000015"))
        assertFalse(Positions.fits(null, ""))
        assertTrue(Positions.fits("0000000015", null))
    }
}
