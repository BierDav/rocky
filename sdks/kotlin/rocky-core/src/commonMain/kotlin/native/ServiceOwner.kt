package rocky

import kotlinx.serialization.json.*

/** A control revision the coordination service accepted for this domain. */
data class ServiceControl(val tenant: String, val revision: Long, val control: String, val expiresAtEpochSeconds: Long)

/** A tenant's use of the service's budgets. */
data class ServiceUsage(val openDomains: Int, val closedDomains: Int, val bytes: Long, val registrationsToday: Int)

/**
 * The owner's control of a domain on the coordination service its database composes, signed with the
 * tenant's 32-byte control key. Keep that key like an account secret; it coordinates metadata only and
 * grants no row, key or enrollment right. Registering needs the publisher's grant for the tenant: an issuer
 * signs it itself. Coordination does not grant row access.
 * Persist each `registration`/`id` before calling and retry with it: the service answers a retry identically.
 */
class ServiceOwner internal constructor(private val session: RockySession, tenant: ByteArray) {
    private val tenant = tenant.also { require(it.size == 32) { "a tenant key is 32 bytes" } }
        .joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }

    /**
     * Registers the trusted node keys. Only generic metadata leaves the device.
     */
    suspend fun register(
        registration: String,
        trusted: List<String>,
        lifetimeSeconds: Long = 86_400,
        admission: JsonObject? = null,
        grant: JsonObject? = null,
    ): ServiceControl = control(
        session.client.call(
            request("coordination_register") {
                put("tenant", tenant)
                put("registration", handleBytes(registration))
                put("trusted", JsonArray(trusted.map(::JsonPrimitive)))
                put("lifetime", lifetimeSeconds)
                admission?.let { put("admission", it) }
                grant?.let { put("grant", it) }
            },
        ),
    )

    /** On an issuer: the publisher's grant letting this tenant key register the application's domains. */
    suspend fun coordinationGrant(lifetimeSeconds: Long = 3_600): JsonObject = session.client.call(
        request("coordination_grant") {
            put("tenant_secret", tenant)
            put("lifetime", lifetimeSeconds)
        },
    ).getValue("grant").jsonObject

    /** The next control revision updates trusted nodes and preserves the committed log. */
    suspend fun control(
        revision: Long,
        id: String,
        trusted: List<String>,
        lifetimeSeconds: Long = 86_400,
    ): ServiceControl = control(
        session.client.call(
            request("coordination_control") {
                put("tenant", tenant)
                put("id", handleBytes(id))
                put("revision", revision)
                put("trusted", JsonArray(trusted.map(::JsonPrimitive)))
                put("lifetime", lifetimeSeconds)
            },
        ),
    )

    /** Permanently closes this domain at its current head and frees its open-domain quota. */
    suspend fun close(id: String): JsonObject = session.client.call(
        request("coordination_close") {
            put("tenant", tenant)
            put("id", handleBytes(id))
        },
    )
    suspend fun usage(): ServiceUsage = session.client.call(request("coordination_usage") { put("tenant", tenant) }).let {
        ServiceUsage(it.int("open_domains"), it.int("closed_domains"), it.getValue("bytes").jsonPrimitive.long, it.int("registrations_today"))
    }
    private fun control(value: JsonObject) = ServiceControl(
        value.getValue("tenant").jsonPrimitive.content,
        value.getValue("revision").jsonPrimitive.long,
        value.getValue("control").jsonPrimitive.content,
        value.getValue("expires").jsonPrimitive.long,
    )
    private fun JsonObject.int(name: String) = getValue(name).jsonPrimitive.int
}
