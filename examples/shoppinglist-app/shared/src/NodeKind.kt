package org.example.examples.shopping

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
    Partial("Partial", "Holds only the lists shared with it"),
    ;

    /**
     * What the device may do: members read devices, which only trusted devices write, and the unused `keys`, publish their own
     * command identity, ask for approvals and read decisions. `coordination` holds the tenant key: trusted only.
     */
    val profile: GrantProfile get() = when (this) {
        Trusted -> GrantProfile.Issuer

        Full, Partial -> GrantProfile.Grants(
            listOf("lists", "devices", "keys", "decisions").map { Grant(it, setOf(Right.Read)) } +
                listOf("categories", "items").map { Grant(it, setOf(Right.Read, Right.Insert, Right.Update, Right.Delete)) } +
                Grant("identities", setOf(Right.Read, Right.Insert, Right.Update)) +
                // A command's row effects need the caller's right too; SQL never writes a command-controlled table.
                Grant("requests", setOf(Right.Read, Right.Insert, Right.Invoke)),
        )
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
 * device joins with. It is an [AppLink], `rocky-shopping://pair?kind=<kind>&name=<name>&invitation=<rocky://join/…>`,
 * so a camera app opens it in the shopping app.
 */
data class PairingCode(val kind: NodeKind, val name: String, val invitation: String) {
    override fun toString(): String = AppLink(ACTION, linkedMapOf("kind" to kind.name.lowercase(), "name" to name, "invitation" to invitation)).toString()

    companion object {
        const val ACTION = "pair"
        fun parse(code: String): PairingCode {
            val parameters = AppLink.parse(code)?.takeIf { it.action == ACTION }?.parameters
                ?: throw IllegalArgumentException("not a shopping pairing code")
            val kind = requireNotNull(parameters["kind"]?.let(NodeKind::of)) { "unknown device kind ${parameters["kind"]}" }
            val invitation = requireNotNull(parameters["invitation"]) { "the pairing code carries no invitation" }
            return PairingCode(kind, parameters["name"].orEmpty(), invitation)
        }
    }
}
