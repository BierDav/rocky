package rocky

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class KeyEpochTest {
    @Test fun siblingsChooseLowerHashesAndNewerGenerationsWin() {
        val siblings = listOf("f", "0", "a", "5").map { KeyEpoch(2, it.repeat(64)) }
        assertEquals(siblings[1], siblings.maxOrNull())
        assertEquals(siblings[1], siblings.reversed().maxOrNull())
        assertEquals(KeyEpoch(3, "f".repeat(64)), (siblings + KeyEpoch(3, "f".repeat(64))).maxOrNull())
    }

    @Test fun completeIdentitiesRoundTripAndMalformedHashesFail() {
        val epoch = KeyEpoch(12, "0123456789abcdef".repeat(4))
        assertEquals(epoch, KeyEpoch.fromJson(epoch.json()))
        assertEquals("12.${epoch.secretHash}", epoch.toString())
        assertFailsWith<IllegalArgumentException> { KeyEpoch(0, "0".repeat(64)) }
        assertFailsWith<IllegalArgumentException> { KeyEpoch(1, "0".repeat(63)) }
    }
}
