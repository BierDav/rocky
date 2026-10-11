package org.example.examples.shopping.data.household

import kotlinx.serialization.json.*
import org.example.examples.shopping.util.hex
import org.example.examples.shopping.util.sha256
import rocky.CommandIdentity
import rocky.Device
import rocky.GrantProfile

/** Registry command IDs plus user keys form the coordinator identity in this application's current generation. */
internal fun Device.commandIdentity(generation: CommandIdentity): CommandIdentity {
    val command = requireNotNull(command) { "The registry device has no command identity" }
    val prefix = "rocky.secure-principal.v1".encodeToByteArray()
    val length = ByteArray(4) { (prefix.size shr (8 * it)).toByte() }
    val principal = sha256(length + prefix + user.chunked(2).map { it.toInt(16).toByte() }.toByteArray()).copyOf(16).hex()
    val identity = Json.parseToJsonElement(generation.encoded).jsonObject.toMutableMap()
    identity["node"] = JsonPrimitive(command)
    identity["key"] = JsonPrimitive(node)
    identity["principal"] = JsonPrimitive(principal)
    identity["handler"] = JsonPrimitive(profile == GrantProfile.Issuer)
    return CommandIdentity.decode(JsonObject(identity).toString())
}
