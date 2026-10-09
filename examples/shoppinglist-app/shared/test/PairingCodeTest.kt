package org.example.examples.shopping

import rocky.GrantProfile
import rocky.ReplicaRole
import kotlin.test.*

class PairingCodeTest {
    @Test fun `pairing codes keep the kind, the name and the invitation`() {
        for (kind in NodeKind.entries) {
            val code = PairingCode(kind, "Kitchen tablet: 2 & more", "rocky://join/abc?x=1&y=%2F")
            assertTrue(code.toString().startsWith("rocky-shopping://pair?kind=${kind.name.lowercase()}&"))
            assertEquals(code, PairingCode.parse(" $code\n"))
        }
        assertEquals(
            AppLink("drive", mapOf("add" to "Milk & eggs", "step" to "a-1")),
            AppLink.parse("rocky-shopping://drive?add=Milk+%26+eggs&step=a-1"),
        )
        assertNull(AppLink.parse("https://example.org/pair"))
        assertFailsWith<IllegalArgumentException> { PairingCode.parse("rocky-shopping://drive?kind=full&invitation=x") }
        assertFailsWith<IllegalArgumentException> { PairingCode.parse("rocky://join/abc") }
        assertEquals(GrantProfile.Issuer, NodeKind.Trusted.profile)
        assertEquals(ReplicaRole.Full, NodeKind.Trusted.role)
        assertEquals(ReplicaRole.Full, NodeKind.Full.role)
        assertEquals(ReplicaRole.Partial, NodeKind.Partial.role)
    }
}
