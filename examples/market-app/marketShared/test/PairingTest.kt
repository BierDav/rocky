package org.example.examples.market

import kotlinx.serialization.json.*
import kotlin.io.encoding.Base64
import kotlin.test.*

class PairingTest {
    private val codec = Base64.UrlSafe.withPadding(Base64.PaddingOption.ABSENT_OPTIONAL)
    private val prefix = "rocky://join/"
    private fun uri(value: JsonObject) = prefix + codec.encode(value.toString().encodeToByteArray())
    private fun decode(uri: String) = Json.parseToJsonElement(codec.decode(uri.removePrefix(prefix)).decodeToString()).jsonObject
    private val invitation = Json.parseToJsonElement(
        """{"v":4,"token":"one-use-token","publisher":"issuer","expires":123,"profile":{"kind":"buyer"},"routes":[{"transport":"iroh","address":"{\"id\":\"endpoint\",\"addrs\":[{\"Ip\":\"127.0.0.1:4000\"},{\"Ip\":\"10.0.2.15:4000\"}]}"}]}""",
    ).jsonObject

    @Test fun aDirectRoutePreservesTheEnrollmentAndEndpointIdentity() {
        val selected = decode(directInvitation(uri(invitation), " 10.0.2.2:5000 "))
        assertEquals(invitation.filterKeys { it != "routes" }, selected.filterKeys { it != "routes" })
        val route = selected.getValue("routes").jsonArray.single().jsonObject
        assertEquals("iroh", route.getValue("transport").jsonPrimitive.content)
        val endpoint = Json.parseToJsonElement(route.getValue("address").jsonPrimitive.content).jsonObject
        assertEquals("endpoint", endpoint.getValue("id").jsonPrimitive.content)
        assertEquals(Json.parseToJsonElement("""[{"Ip":"10.0.2.2:5000"}]"""), endpoint.getValue("addrs"))
    }

    @Test fun aManualIpDoesNotDiscardTheIssuersRelay() {
        val route = invitation.getValue("routes").jsonArray.single().jsonObject
        val endpoint = Json.parseToJsonElement(route.getValue("address").jsonPrimitive.content).jsonObject
        val relay = buildJsonObject { put("Relay", "https://relay.example/") }
        val relayed = JsonObject(endpoint + ("addrs" to JsonArray(endpoint.getValue("addrs").jsonArray + relay)))
        val source = JsonObject(invitation + ("routes" to JsonArray(listOf(JsonObject(route + ("address" to JsonPrimitive(relayed.toString())))))))
        val selected = decode(directInvitation(uri(source), "10.0.2.2:5000"))
        val selectedEndpoint = Json.parseToJsonElement(selected.getValue("routes").jsonArray.single().jsonObject.getValue("address").jsonPrimitive.content).jsonObject
        assertEquals(JsonArray(listOf(buildJsonObject { put("Ip", "10.0.2.2:5000") }, relay)), selectedEndpoint.getValue("addrs"))
        assertEquals(source.filterKeys { it != "routes" }, selected.filterKeys { it != "routes" })
    }

    @Test fun invalidAddressesAndOtherInvitationVersionsAreRefused() {
        for (address in listOf("localhost", ":5000", "127.0.0.1:0", "127.0.0.1:65536", "host name:5000")) {
            assertFailsWith<IllegalArgumentException> { directInvitation(uri(invitation), address) }
        }
        assertFailsWith<IllegalArgumentException> { directInvitation(uri(JsonObject(invitation + ("v" to JsonPrimitive(3)))), "127.0.0.1:5000") }
    }
}
