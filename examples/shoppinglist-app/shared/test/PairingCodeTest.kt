package org.example.examples.shopping

import kotlin.test.*
import rocky.GrantProfile
import rocky.ReplicaRole

class PairingCodeTest {
    @Test fun `pairing codes keep the kind, the name and the invitation`() {
        for (kind in NodeKind.entries) {
            val code = PairingCode(kind, "Kitchen tablet: 2", "rocky://join/abc")
            assertEquals(code, PairingCode.parse(code.toString()))
        }
        assertFailsWith<IllegalArgumentException> { PairingCode.parse("rocky://join/abc") }
        assertEquals(GrantProfile.Issuer, NodeKind.Trusted.profile)
        assertEquals(ReplicaRole.Full, NodeKind.Trusted.role)
        assertEquals(ReplicaRole.Full, NodeKind.Full.role)
        assertEquals(ReplicaRole.Partial, NodeKind.Partial.role)
    }
}
