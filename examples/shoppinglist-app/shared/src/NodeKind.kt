package org.example.examples.shopping

import kotlin.io.encoding.Base64
import rocky.Grant
import rocky.GrantProfile
import rocky.ReplicaRole
import rocky.Right

/**
 * What a new device is to the household, chosen by the trusted device that invites it.
 * See `docs/runtime/shopping-devices.md#device-kinds`.
 */
enum class NodeKind(val label: String, val summary: String) {
    /** An issuer: creates lists, invites devices and receives every list. */
    Trusted("Trusted", "Creates lists, adds devices and holds every list"),
    /** Holds and edits every list, receiving each list's key automatically. */
    Full("Full", "Holds and edits every list"),
    /** Holds only the lists a trusted device shares with it. */
    Partial("Partial", "Holds only the lists shared with it");

    /** What the device may do: members read devices and keys, which only trusted devices write. */
    val profile: GrantProfile get() = when (this) {
        Trusted -> GrantProfile.Issuer
        Full, Partial -> GrantProfile.Grants(
            listOf("lists", "devices", "keys").map { Grant(it, setOf(Right.Read)) } +
                listOf("categories", "items").map { Grant(it, setOf(Right.Read, Right.Insert, Right.Update, Right.Delete)) })
    }
    val role: ReplicaRole get() = if (this == Partial) ReplicaRole.Partial else ReplicaRole.Full
    /** Trusted devices send every list's key to devices of this kind. */
    val receivesEveryList: Boolean get() = this != Partial

    companion object {
        fun of(name: String): NodeKind? = entries.firstOrNull { it.name.equals(name, ignoreCase = true) }
    }
}

/**
 * What a pairing QR code carries: the kind and name the inviting device chose, and the Rocky invitation the new
 * device joins with. Format: `rocky-shopping:<kind>:<base64url name>:<rocky://join/…>`.
 */
data class PairingCode(val kind: NodeKind, val name: String, val invitation: String) {
    override fun toString(): String = "$PREFIX:${kind.name.lowercase()}:${Base64.UrlSafe.encode(name.encodeToByteArray())}:$invitation"

    companion object {
        private const val PREFIX = "rocky-shopping"
        fun parse(code: String): PairingCode {
            val parts = code.trim().split(":", limit = 4)
            require(parts.size == 4 && parts[0] == PREFIX) { "not a shopping pairing code" }
            val kind = requireNotNull(NodeKind.of(parts[1])) { "unknown device kind ${parts[1]}" }
            return PairingCode(kind, Base64.UrlSafe.decode(parts[2]).decodeToString(), parts[3])
        }
    }
}
