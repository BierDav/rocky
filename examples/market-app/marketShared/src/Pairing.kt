package org.example.examples.market

import kotlinx.serialization.json.*
import kotlin.io.encoding.Base64

/** Selects one IP route while retaining the issuer's relay and authenticated endpoint identity. */
internal fun directInvitation(invitation: String, address: String): String {
    val target = address.trim()
    val host = target.substringBeforeLast(':', "")
    val port = target.substringAfterLast(':', "").toIntOrNull()
    require(host.isNotBlank() && target.none { it.isWhitespace() } && port != null && port in 1..65535) { "Enter a host:port direct pairing address" }
    val prefix = "rocky://join/"
    require(invitation.startsWith(prefix)) { "Invalid pairing invitation" }
    val codec = Base64.UrlSafe.withPadding(Base64.PaddingOption.ABSENT_OPTIONAL)
    val value = Json.parseToJsonElement(codec.decode(invitation.removePrefix(prefix)).decodeToString()).jsonObject
    require(value.getValue("v").jsonPrimitive.int == 4) { "Unsupported invitation version" }
    val routes = value.getValue("routes").jsonArray.filter { it.jsonObject.getValue("transport").jsonPrimitive.content == "iroh" }
    require(routes.size == 1) { "Direct pairing needs one iroh endpoint" }
    val route = routes.single().jsonObject
    val endpoint = Json.parseToJsonElement(route.getValue("address").jsonPrimitive.content).jsonObject
    val selected = JsonObject(
        endpoint +
            (
                "addrs" to buildJsonArray {
                    add(buildJsonObject { put("Ip", target) })
                    endpoint.getValue("addrs").jsonArray.filter { "Relay" in it.jsonObject }.forEach { add(it) }
                }
                ),
    )
    val routed = JsonObject(route + ("address" to JsonPrimitive(selected.toString())))
    val result = JsonObject(value + ("routes" to JsonArray(listOf(routed))))
    return prefix + codec.encode(result.toString().encodeToByteArray())
}
