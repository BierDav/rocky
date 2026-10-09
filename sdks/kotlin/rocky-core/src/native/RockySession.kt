package rocky

import java.io.File
import java.security.SecureRandom
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*

/** One enrolled device's secure database: the native engine, its enrollment and background sync. */
open class RockySession internal constructor(internal val client: RockyClient, override val domain: String, val isIssuer: Boolean) : RuntimePort, CommandPort {
    private val lifetime = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var synchronization: Job? = null
    private val connection = Mutex()
    private val closing = Mutex()
    // One SQL transaction at a time on this session.
    private val writing = Mutex()
    private var closed = false
    private val mutableStatus = MutableStateFlow(ConnectionStatus(ConnectionState.Unavailable))
    val status: StateFlow<ConnectionStatus> = mutableStatus.asStateFlow()
    val credentials: Credentials get() = Credentials(this)
    val partitions: Partitions get() = client.partitions
    val horizons: Horizons get() = client.horizons
    val commands: CommandPort get() = this
    fun readiness(): Flow<JsonObject> = client.readiness()
    /** `committed` tells whether a write committed under `handle`. */
    suspend fun reconcile(handle: String): JsonObject = client.recoverMutation(handleBytes(handle))
    /** Persisted pruning policy for one granted collection; at least one other copy is always required. */
    suspend fun retention(collection: String, policy: JsonObject? = null, maintain: Boolean = false): JsonObject =
        client.call(request("retention") { put("collection", collection); policy?.let { put("policy", it) }; put("maintain", maintain) })
    /** Bounds apply to this open database; set them again after reopening. */
    suspend fun retentionLimits(limits: JsonObject? = null): JsonObject =
        client.call(request("retention_limits") { limits?.let { put("limits", it) } })
    suspend fun generation(): JsonObject = client.call(request("generation"))
    override fun observe(query: JsonObject): Flow<JsonObject> = client.observe(query)
    override suspend fun read(query: JsonObject): JsonObject = client.read(query)
    override suspend fun <T> write(handle: String, block: suspend SqlWrite.() -> T): T = writing.withLock {
        val committed = handleBytes(handle)
        val transaction = object : SqlWrite {
            override suspend fun nextId(table: String): Long = client.nextId(table).toLong()
            override suspend fun nextId(table: String, vararg partitionValues: Any?): Long =
                client.nextId(table, JsonArray(partitionValues.map(::cell))).toLong()
            override suspend fun execute(sql: String, vararg values: Any?): Long =
                client.sql(sql, JsonArray(values.map(::cell))).text("changes").toLong()
        }
        client.sql("BEGIN")
        try {
            transaction.block().also { client.call(request("sql") { put("sql", "COMMIT"); put("handle", committed) }) }
        } catch (error: Throwable) {
            // Fails harmlessly when the transaction already ended.
            withContext(NonCancellable) { runCatching { client.sql("ROLLBACK") } }
            throw error
        }
    }
    /** Moves a parent and its cascade children in one commit, outside an open SQL/Room transaction. */
    suspend fun moveRow(handle: String, table: String, id: Long, from: List<Any?>, to: List<Any?>): JsonObject = writing.withLock {
        client.moveRow(handleBytes(handle), table, id.toString(), JsonArray(from.map(::cell)), JsonArray(to.map(::cell)))
    }
    /** A fresh invitation for one device with `profile`, replacing the previous one. Only an issuer invites. */
    suspend fun pairing(profile: GrantProfile): PairingInvitation {
        val result = client.call(request("pairing") { put("profile", profile.json()) })
        return PairingInvitation(result.text("uri"), result.text("expires").toLong(), profile(result.getValue("profile").jsonObject))
    }
    /**
     * Starter pairing of another device of this user. By default it becomes an issuer too, with its own keys and author
     * numbers, so losing either device locks nobody out; `issuing = false` pairs a writer of every table instead.
     */
    suspend fun pairDevice(issuing: Boolean = true): PairingInvitation = pairing(
        if (issuing) GrantProfile.Issuer
        else GrantProfile.Writer(client.call(request("domain")).getValue("collections").jsonArray.map { it.jsonPrimitive.content }.toSet()))
    suspend fun session(): SessionStatus = status(client.call(request("session")))
    /** Fresh credentials with the same grants from any issuer; a revoked device is refused. */
    suspend fun renew(): SessionStatus = status(client.call(request("renew")))
    private fun status(result: JsonObject) = SessionStatus(result.text("principal"), result.text("node"), result.text("epoch").toLong(),
        result.text("expires").toLong(), result.getValue("issuer").jsonPrimitive.boolean, profile(result.getValue("profile").jsonObject), result.text("transport"))
    /** The devices this issuer enrolled or renewed. */
    /**
     * Issues `profile` to the device whose public keys `device` names, as `Credentials.prepare` reported them. Returns
     * the bundle that device passes to `RockyDatabase.install`. It carries reader keys: hand it over like a secret.
     * The device is then known like an enrolled one. Only an issuer grants. `delegations` above 1 lets the device
     * delegate further itself. `authors` reserves that many numbers for the recipient to assign pseudonyms.
     */
    suspend fun grant(device: JsonObject, profile: GrantProfile, ttlSeconds: Long? = null, delegations: Int? = null, authors: Int? = null): JsonObject =
        issue("grant", device, profile, ttlSeconds, delegations, authors)
    /**
     * Delegates `profile`, which must narrow this device's own grants, to a prepared device. Any device whose credentials
     * allow a further delegation does so; the delegate receives no more rights, tables, columns or lifetime than this
     * device holds, and renews by being delegated to again and calling `install`.
     */
    suspend fun delegate(device: JsonObject, profile: GrantProfile, ttlSeconds: Long? = null, delegations: Int? = null, authors: Int? = null): JsonObject =
        issue("delegate", device, profile, ttlSeconds, delegations, authors)
    private suspend fun issue(kind: String, device: JsonObject, profile: GrantProfile, ttlSeconds: Long?, delegations: Int?, authors: Int?): JsonObject =
        client.call(request(kind) {
            put("device", device)
            if (profile is GrantProfile.Grants) put("grants", grants(profile.grants)) else put("profile", profile.json())
            ttlSeconds?.let { put("ttl", it) }
            delegations?.let { put("delegations", it) }
            authors?.let { put("authors", it) }
        }).getValue("supplied").jsonObject
    /** Replaces this device's credentials with a supplied bundle, such as a renewed delegation. They are verified first. */
    suspend fun install(supplied: JsonObject): SessionStatus = status(client.call(request("install") { put("supplied", supplied) }))
    suspend fun devices(): List<Device> = client.call(request("devices")).getValue("devices").jsonArray.map { it.jsonObject }.map {
        Device(it.text("node"), it.text("user"), it.text("enrolled").toLong(), it.getValue("revoked").jsonPrimitive.boolean, profile(it.getValue("profile").jsonObject))
    }
    /** This device's contact for another enrolled device to add. Needs an attached network that accepts. */
    suspend fun contact(): Contact = client.call(request("contact")).let { Contact(it.text("uri"), it.text("node"), routes(it)) }
    /** Routes a preissued invitation or issuer contact through a custody peer; grants stay unchanged. */
    suspend fun routeEnrollment(uri: String, custodyContact: String): String =
        client.call(request("enrollment_route") { put("uri", uri); put("contact", custodyContact) }).text("uri")
    /**
     * Configures the device a contact names as a peer to sync with and serve, or learns its new routes.
     * Both devices add each other. Its credentials are still checked on every session.
     */
    suspend fun addPeer(contact: String): String = client.call(request("peer_add") { put("contact", contact) }).text("node")
    /** Forgets a peer. A member keeps its last issuer, which renews its credentials. */
    suspend fun removePeer(node: String) { client.call(request("peer_remove") { put("node", node) }) }
    suspend fun peers(): List<Peer> = client.call(request("peers")).getValue("peers").jsonArray.map { peer(it.jsonObject) }
    /** The network service's state and bounds, independent of the database's. */
    suspend fun network(): NetworkStatus = client.call(request("network")).let { status ->
        val inbound = status["inbound"]?.jsonObject
        NetworkStatus(status.getValue("attached").jsonPrimitive.boolean, status["kind"]?.jsonPrimitive?.content,
            status["owned"]?.jsonPrimitive?.boolean ?: false, inbound?.getValue("active")?.jsonPrimitive?.int ?: 0,
            inbound?.getValue("limit")?.jsonPrimitive?.int ?: 0, inbound?.text("served")?.toLong() ?: 0, inbound?.text("refused")?.toLong() ?: 0,
            status.getValue("peers").jsonArray.map { peer(it.jsonObject) })
    }
    /**
     * Ends a device's authority at the next policy epoch and starts a new key epoch for the tables it read;
     * remaining devices receive it when they renew. Data it already decrypted stays with it. Another issuer, such as a
     * lost creator, is revoked the same way, but keeps the publisher key it holds: revocation recovers from loss, not theft.
     */
    suspend fun revoke(node: String) { client.call(request("revoke") { put("node", node) }) }
    /** Deletes this device's keys and ends its sessions; `purge` also removes its local data on close. */
    suspend fun logout(purge: Boolean = false) { client.call(request("logout") { put("purge", purge) }) }
    suspend fun setOnline(online: Boolean) = connection.withLock {
        client.call(request("connection") { put("online", online) })
        mutableStatus.value = ConnectionStatus(if (online) ConnectionState.Unavailable else ConnectionState.Offline)
    }
    suspend fun syncOnce(): ConnectionStatus = connection.withLock {
        try {
            val result = client.call(request("sync"))
            mutableStatus.value = ConnectionStatus(if (result.getValue("state").jsonPrimitive.content == "offline") ConnectionState.Offline else ConnectionState.Connected)
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { mutableStatus.value = ConnectionStatus(ConnectionState.Unavailable, error.message) }
        mutableStatus.value
    }
    /** Enables native change-driven sync; cancellation stops this status observer. */
    suspend fun syncContinuously() {
        var result = client.call(request("sync_start"))
        while (currentCoroutineContext().isActive) {
            mutableStatus.value = ConnectionStatus(when (result["state"]?.jsonPrimitive?.content) {
                "connected" -> ConnectionState.Connected
                "offline" -> ConnectionState.Offline
                else -> ConnectionState.Unavailable
            }, result["error"]?.jsonPrimitive?.contentOrNull)
            result = client.call(request("sync_next") { put("after", result.getValue("token")) })
        }
    }
    /** The owner's control of this domain on its coordination service, with the tenant's control key. */
    fun service(tenant: ByteArray): ServiceOwner = ServiceOwner(this, tenant)
    /** This device as a coordinator names it, to register it as an executor or a caller. */
    suspend fun commandIdentity(): CommandIdentity = commandIdentity(client.call(request("command_identity")))
    override suspend fun <I, A, R> submit(command: Command<I, A, R>, arguments: I, handle: String): Ticket<A, R> =
        command.ticket(client.call(command.submission(arguments, handle)))
    override suspend fun <I, A, R> ticket(command: Command<I, A, R>, handle: String): Ticket<A, R>? =
        client.call(request("command_ticket") { put("handle", handleBytes(handle)) })
            .takeIf { it.getValue("state").jsonPrimitive.content != "unresolved" }?.let { command.ticket(it) }
    override suspend fun <I, A, R> cancel(command: Command<I, A, R>, handle: String): Ticket<A, R> =
        command.ticket(client.call(request("command_cancel") { put("handle", handleBytes(handle)) }))
    override suspend fun <I, A, R> run(command: Command<I, A, R>, handle: String): Ticket<A, R> =
        command.ticket(client.call(request("command_run") { put("handle", handleBytes(handle)) }).getValue("tickets").jsonArray.single().jsonObject)
    /**
     * Tells the coordinator that this device's settled tickets reached whoever needs their results, so it
     * retires those identities and frees their response slots. Without it, at most 32 identities per device
     * stay outstanding until their admission window passes, and further runs fail with `quota`.
     */
    suspend fun acknowledgeCommands() { client.call(request("command_acknowledge")) }
    /** One pass over every unresolved ticket; returns how many remain unresolved. */
    suspend fun runCommands(): Int = client.call(request("command_run")).getValue("tickets").jsonArray
        .count { it.jsonObject.getValue("state").jsonPrimitive.content !in setOf("confirmed", "applied", "cancelled", "retired", "expired") }
    /** The confirmed rows of a command's table, separate from the mergeable table of the same name. */
    suspend fun confirmedRows(table: String): ConfirmedRows = confirmedRows(client.call(request("command_rows") { put("table", table) }))
    /**
     * For an application that forwards commands itself: the signed invocation of a ticket, an executor's answer
     * to one, and settling the ticket with that answer. Whoever forwards the bytes gains no authority over them.
     */
    suspend fun envelope(handle: String): ByteArray = client.call(request("command_envelope") { put("handle", handleBytes(handle)) }).blob("envelope")
    suspend fun execute(envelope: ByteArray): ByteArray = client.call(request("command_execute") { put("envelope", bytes(envelope)) }).blob("answer")
    suspend fun <I, A, R> settle(command: Command<I, A, R>, handle: String, answer: ByteArray): Ticket<A, R> =
        command.ticket(client.call(request("command_settle") { put("handle", handleBytes(handle)); put("answer", bytes(answer)) }))
    private fun JsonObject.blob(name: String): ByteArray = getValue(name).jsonArray.let { array -> ByteArray(array.size) { array[it].jsonPrimitive.int.toByte() } }
    suspend fun connect() = setOnline(true)
    suspend fun disconnect() = setOnline(false)
    open suspend fun close() = withContext(NonCancellable) { closing.withLock {
        if (!closed) {
            closed = true
            lifetime.cancel()
            synchronization?.join()
            client.close()
        }
    } }
    companion object {
        private fun peer(value: JsonObject) = Peer(value.text("node"), value.getValue("issuer").jsonPrimitive.boolean, routes(value),
            value.text("state"), value.getValue("checked").jsonPrimitive.contentOrNull?.toLong(), value.getValue("error").jsonPrimitive.contentOrNull)
        private fun routes(value: JsonObject) = value.getValue("routes").jsonArray.map { it.jsonObject }.map { Route(it.text("transport"), it.text("address")) }
        private fun JsonObject.text(name: String): String = getValue(name).jsonPrimitive.content
        internal fun GrantProfile.json(): JsonObject = buildJsonObject {
            when (this@json) {
                is GrantProfile.Custody -> { put("kind", "custody"); put("scopes", JsonArray(tables.map(::JsonPrimitive))) }
                is GrantProfile.Reader -> { put("kind", "reader"); put("scopes", JsonArray(tables.map(::JsonPrimitive))) }
                is GrantProfile.Writer -> { put("kind", "writer"); put("scopes", JsonArray(tables.map(::JsonPrimitive))) }
                GrantProfile.Issuer -> put("kind", "issuer")
                is GrantProfile.Grants -> { put("kind", "grants"); put("grants", grants(grants)) }
            }
        }
        internal fun grants(grants: List<Grant>) = JsonArray(grants.map { grant ->
            buildJsonObject {
                put("scope", grant.table)
                put("rights", JsonArray(grant.rights.map { JsonPrimitive(it.wire) }))
                grant.columns?.let { put("columns", JsonArray(it.map(::JsonPrimitive))) }
            }
        })
        internal fun profile(value: JsonObject): GrantProfile {
            fun tables() = value.getValue("scopes").jsonArray.map { it.jsonPrimitive.content }.toSet()
            return when (val kind = value.text("kind")) {
                "custody" -> GrantProfile.Custody(tables())
                "reader" -> GrantProfile.Reader(tables())
                "writer" -> GrantProfile.Writer(tables())
                "issuer" -> GrantProfile.Issuer
                "grants" -> GrantProfile.Grants(value.getValue("grants").jsonArray.map { it.jsonObject }.map { grant ->
                    Grant(grant.text("scope"), grant.getValue("rights").jsonArray.map { Right.of(it.jsonPrimitive.content) }.toSet(),
                        grant["columns"]?.jsonArray?.map { it.jsonPrimitive.content }?.toSet())
                })
                else -> throw IllegalStateException("unknown grant profile $kind")
            }
        }
        fun newHandle(): String {
            val bytes = ByteArray(32).also { SecureRandom().nextBytes(it) }
            return bytes.joinToString("") { "%02x".format(it.toInt() and 255) }
        }
    }
}
