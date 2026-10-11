package org.example.examples.shopping

import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.presentation.navigation.destinationOf
import org.example.examples.shopping.presentation.screen.pairing.PairingOfferKey
import kotlin.test.*

class DeepLinksTest {
    @Test fun pairingLinksOpenAnOfferAndOtherLinksNothing() {
        val code = PairingCode(NodeKind.Partial, "Kitchen", "rocky://join/abc")
        assertEquals(PairingOfferKey(code.toString()), destinationOf(" $code\n"))
        assertNull(destinationOf("rocky-shopping://drive?sync=1"))
        assertNull(destinationOf("rocky-shopping://pair?kind=full"))
        assertNull(destinationOf("https://example.org/pair"))
    }
}
