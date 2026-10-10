package rocky

import kotlinx.serialization.json.*

/** A control revision the coordination service accepted for this domain. */
data class ServiceControl(val tenant: String, val revision: Long, val control: String)

/** A tenant's use of the service's budgets. */
data class ServiceUsage(val openDomains: Int, val closedDomains: Int, val bytes: Long, val registrationsToday: Int)

/**
 * The owner's control of a domain on the coordination service its database composes, signed with the
 * tenant's 32-byte control key. Keep that key like an account secret; it coordinates metadata only and
 * grants no row, key or enrollment right. Registering needs the publisher's grant for the tenant: an issuer
 * signs it itself, or once with `coordinationGrant` for any device holding `Execute`, which then registers.
 * Persist each `registration`/`id` before calling and retry with it: the service answers a retry identically.
 */
class ServiceOwner internal constructor(private val session: RockySession, tenant: ByteArray) {
    private val tenant = tenant.also { require(it.size == 32) { "a tenant key is 32 bytes" } }
        .joinToString("") { "%02x".format(it.toInt() and 255) }

    /**
     * Registers the domain: who may execute and who may invoke. Nothing of the schema or rows is sent. A device that
     * is not an issuer passes the `grant` an issuer's `coordinationGrant` returned, and must hold `Execute`.
     */
    suspend fun register(
        registration: String,
        executors: List<CommandIdentity>,
        invokers: List<CommandIdentity>,
        lifetimeSeconds: Long = 86_400,
        admission: JsonObject? = null,
        grant: JsonObject? = null,
    ): ServiceControl = control(
        session.client.call(
            request("command_service_register") {
                put("tenant", tenant)
                put("registration", handleBytes(registration))
                put("executors", identities(executors))
                put("invokers", identities(invokers))
                put("lifetime", lifetimeSeconds)
                admission?.let { put("admission", it) }
                grant?.let { put("grant", it) }
            },
        ),
    )

    /** On an issuer: the publisher's grant letting this tenant key register the application's domains. */
    suspend fun coordinationGrant(lifetimeSeconds: Long = 3_600): JsonObject = session.client.call(
        request("command_coordination_grant") {
            put("tenant_secret", tenant)
            put("lifetime", lifetimeSeconds)
        },
    ).getValue("grant").jsonObject

    /** The next control revision. It fences the current executor and keeps the head; an eligible executor acquires again. */
    suspend fun control(
        revision: Long,
        id: String,
        executors: List<CommandIdentity>,
        invokers: List<CommandIdentity>,
        lifetimeSeconds: Long = 86_400,
    ): ServiceControl = control(
        session.client.call(
            request("command_service_control") {
                put("tenant", tenant)
                put("id", handleBytes(id))
                put("revision", revision)
                put("executors", identities(executors))
                put("invokers", identities(invokers))
                put("lifetime", lifetimeSeconds)
            },
        ),
    )

    /**
     * Activates this device's verified generation at the service's exact head, after a declared upgrade.
     * Returns false when it was already active. The device must hold every confirmed result first.
     */
    suspend fun activate(id: String): Boolean = session.client.call(
        request("command_service_activate") {
            put("tenant", tenant)
            put("id", handleBytes(id))
        },
    ).let { it.getValue("active").jsonPrimitive.boolean && it["sequence"] != null }

    /** Permanently closes this domain at its current head and frees its open-domain quota. */
    suspend fun close(id: String): JsonObject = session.client.call(
        request("command_service_close") {
            put("tenant", tenant)
            put("id", handleBytes(id))
        },
    )
    suspend fun usage(): ServiceUsage = session.client.call(request("command_service_usage") { put("tenant", tenant) }).let {
        ServiceUsage(it.int("open_domains"), it.int("closed_domains"), it.getValue("bytes").jsonPrimitive.long, it.int("registrations_today"))
    }
    private fun identities(list: List<CommandIdentity>) = JsonArray(list.map { Json.parseToJsonElement(it.json) })
    private fun control(value: JsonObject) = ServiceControl(
        value.getValue("tenant").jsonPrimitive.content,
        value.getValue("revision").jsonPrimitive.long,
        value.getValue("control").jsonPrimitive.content,
    )
    private fun JsonObject.int(name: String) = getValue(name).jsonPrimitive.int
}
