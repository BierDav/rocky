package rocky

import kotlinx.coroutines.*
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import rocky.internal.native.NativeConnection
import rocky.internal.native.NativeDatabase
import rocky.internal.native.NativeDatabaseSession
import rocky.internal.native.NativeException
import rocky.internal.native.NativeNetwork
import rocky.internal.native.NativeNetworkService
import rocky.internal.native.NativeTableEvents
import rocky.internal.native.prepareDevice

/** Database ownership is independent of sessions and of an optional network. */
class RockyDatabase private constructor(
    private val native: NativeDatabase,
    client: RockyClient,
    domain: String,
    issuer: Boolean,
) : RockySession(client, domain, issuer) {
    private val lifetime = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val lifecycle = Mutex()
    private var closed = false
    private var automaticSync = false
    private var synchronizing: Job? = null

    /** The service attached by create/join/open; closing it leaves the database open. */
    val openedNetwork: RockyNetworkService? = native.openedNetwork()?.let(::RockyNetworkService)

    /** Each session owns its observations and cancellation, while sharing committed storage. */
    fun openSession(): RockySession = RockySession(RockyClient(SessionTransport(native.session(), domain)), domain, isIssuer)
    suspend fun attach(transport: PeerTransport): RockyNetworkService {
        currentCoroutineContext().ensureActive()
        var attached: RockyNetworkService? = null
        try {
            val service = withContext(NonCancellable) { nativeCall { RockyNetworkService(withNetwork(transport) { native.attach(it ?: error("missing network")) }) } }
            attached = service
            currentCoroutineContext().ensureActive()
            return service
        } catch (error: Throwable) {
            withContext(NonCancellable) { attached?.close() }
            throw error
        }
    }

    /**
     * For a process the platform may freeze or kill: cancels the native automatic pass promptly, so none
     * runs after this returns, and stops local-network discovery. The file, sessions, attached network and its
     * endpoint stay open, and reads and writes continue; acknowledged writes are already durable.
     */
    suspend fun enterBackground() = lifecycle.withLock {
        setAutomaticSync(false)
        synchronizing?.cancelAndJoin()
        synchronizing = null
        client.call(request("network_background"))
    }

    /** Resumes automatic sync, starting with an immediate pass, if the database was opened with it. */
    suspend fun enterForeground() = lifecycle.withLock {
        client.call(request("network_foreground"))
        if (automaticSync && synchronizing == null) {
            val status = setAutomaticSync(true)
            synchronizing = lifetime.launch { observeSync(status) }
        }
    }

    /**
     * The native handles of a SQL driver, such as `rocky-sqlite`'s: a writer is a secure SQL session, a reader
     * steps snapshot cursors. See `docs/runtime/sql-driver.md`.
     */
    suspend fun driverConnection(readOnly: Boolean, profile: String? = null, transactionTimeoutMillis: Long? = null): NativeConnection = nativeCall { native.connect(readOnly, profile, transactionTimeoutMillis?.toULong()) }

    /** Committed changes of `tables`, or of every table, for a driver to invalidate what it read. */
    suspend fun driverTableEvents(tables: List<String>? = null): NativeTableEvents = nativeCall { native.observeTables(tables) }
    suspend fun backup(archive: String): JsonObject = nativeCall {
        Json.parseToJsonElement(native.backup(archive)).jsonObject
    }
    override suspend fun close() = withContext(NonCancellable) {
        lifecycle.withLock {
            if (!closed) {
                closed = true
                lifetime.cancel()
                try {
                    super.close()
                } finally {
                    try {
                        nativeCall { native.shutdown() }
                    } finally {
                        openedNetwork?.destroy()
                        native.destroy()
                    }
                }
            }
        }
    }
    companion object : PlatformOpeners() {
        private suspend fun make(automaticSync: Boolean, open: () -> NativeDatabase): RockyDatabase {
            currentCoroutineContext().ensureActive()
            var opened: RockyDatabase? = null
            try {
                val database = withContext(Dispatchers.IO + NonCancellable) {
                    val native = nativeCall { open() }
                    try {
                        val domain = native.scope()
                        val client = RockyClient(SessionTransport(native.session(), domain))
                        val info = client.call(request("domain"))
                        RockyDatabase(native, client, domain, info.getValue("issuer").jsonPrimitive.boolean).also { opened = it }
                    } catch (error: Throwable) {
                        native.closeAfterFailure()
                        throw error
                    }
                }
                currentCoroutineContext().ensureActive()
                if (automaticSync) {
                    database.automaticSync = true
                    database.enterForeground()
                }
                return database
            } catch (error: Throwable) {
                withContext(NonCancellable) { opened?.close() }
                throw error
            }
        }
        private suspend fun NativeDatabase.closeAfterFailure() = withContext(NonCancellable) {
            try {
                shutdown()
            } finally {
                destroy()
            }
        }

        /** A new installation writes at once: its authors are installed before it is returned. */
        private suspend fun RockyDatabase.withAuthors(required: Boolean): RockyDatabase {
            try {
                installAuthors()
            } catch (error: Throwable) {
                // A member whose issuer did not answer asks again on its next sync.
                if (required || error !is RockyException) {
                    close()
                    throw error
                }
            }
            return this
        }

        /** Whether [directory] holds an installation to `open` or `resume`. */
        fun exists(directory: String): Boolean = fileExists("$directory/identity.json")

        /** Local composition: no network is started unless supplied. */
        suspend fun local(directory: String, schema: Schema, secrets: SecretStore? = null, commands: CommandRuntime? = null): RockyDatabase = make(false) { NativeDatabase.create(directory, schema.json(), secretProvider(secrets), null, composeCommands(commands), null) }

        /** Installs an independently supplied credential bundle after Credentials.prepare. */
        suspend fun install(
            directory: String,
            schema: Schema,
            supplied: JsonObject,
            secrets: SecretStore? = null,
            transport: PeerTransport? = null,
            commands: CommandRuntime? = null,
        ): RockyDatabase = make(false) {
            withNetwork(transport) { selected ->
                NativeDatabase.install(
                    directory,
                    schema.json(),
                    supplied.toString(),
                    secretProvider(secrets),
                    selected,
                    composeCommands(commands),
                )
            }
        }

        /** A new domain with this device as its first issuer, with its authors installed. */
        suspend fun create(directory: String, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null): RockyDatabase = make(automaticSync) { withNetwork(transport) { NativeDatabase.create(directory, schema.json(), secretProvider(secrets), it, composeCommands(commands), null) } }
            .withAuthors(required = true)

        /** Enrolls with an invitation and installs the authors of the tables it writes, from its issuer when it does not issue. */
        suspend fun join(directory: String, schema: Schema, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null): RockyDatabase = make(automaticSync) { withNetwork(transport) { NativeDatabase.join(directory, schema.json(), invitation, requireNotNull(it), secretProvider(secrets), composeCommands(commands)) } }
            .let { it.withAuthors(required = it.isIssuer) }

        /** Resumes the installation in [directory], or creates a new domain there. */
        suspend fun openOrCreate(directory: String, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null): RockyDatabase = if (exists(directory)) {
            resume(directory, schema, transport, secrets, automaticSync, commands)
        } else {
            create(directory, schema, transport, secrets, automaticSync, commands)
        }
        suspend fun open(directory: String, schema: Schema, transport: PeerTransport? = null, secrets: SecretStore? = null, automaticSync: Boolean = false, commands: CommandRuntime? = null): RockyDatabase = make(automaticSync) { withNetwork(transport) { NativeDatabase.open(directory, schema.json(), secretProvider(secrets), it, composeCommands(commands)) } }
        suspend fun resume(directory: String, schema: Schema, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, automaticSync: Boolean = true, commands: CommandRuntime? = null): RockyDatabase = open(directory, schema, transport, secrets, automaticSync, commands)
        suspend fun restore(directory: String, schema: Schema, archive: String, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null, commands: CommandRuntime? = null): RockyDatabase = make(false) { withNetwork(transport) { NativeDatabase.restore(directory, schema.json(), archive, invitation, requireNotNull(it), secretProvider(secrets), composeCommands(commands)) } }
        fun newHandle(): String = newMutationHandle()
    }
}

/** Ends only this attached service; borrowed transports and endpoints stay application-owned. */
class RockyNetworkService internal constructor(private val native: NativeNetworkService) {
    internal fun destroy() = native.destroy()
    suspend fun close() = withContext(NonCancellable) {
        try {
            native.shutdown()
        } finally {
            native.destroy()
        }
    }
}

/** A verified author number for this device's separate signing key on one table. */
data class AuthorPseudonym(val collection: String, val author: Int)

/** The issuer's private mapping from an author pseudonym to a device. */
data class PseudonymOwner(val node: String, val collection: String, val author: Int)

/** Credential lifecycle over managed storage, independently usable from command delivery. */
class Credentials internal constructor(private val session: RockySession) {
    companion object : PlatformCredentialOpeners() {
        /** Prepares public user/node keys for an independent issuer; secret material stays in the provider. */
        suspend fun prepare(directory: String, secrets: SecretStore? = null): JsonObject = withContext(Dispatchers.IO) {
            nativeCall { Json.parseToJsonElement(prepareDevice(directory, secretProvider(secrets))).jsonObject }
        }
    }
    suspend fun status(): SessionStatus = session.session()
    suspend fun renew(): SessionStatus = session.renew()
    suspend fun pairing(profile: GrantProfile): PairingInvitation = session.pairing(profile)
    suspend fun invitationOpen(uri: String): Boolean = session.invitationOpen(uri)

    /** Another device of this user, an issuer unless `issuing = false`; see `RockySession.pairDevice`. */
    suspend fun pairDevice(issuing: Boolean = true): PairingInvitation = session.pairDevice(issuing)

    /** Raw grants or a profile for a prepared device; see `RockySession.grant`. */
    suspend fun grant(device: JsonObject, grants: List<Grant>, ttlSeconds: Long? = null, delegations: Int? = null, authors: Int? = null): JsonObject = session.grant(device, GrantProfile.Grants(grants), ttlSeconds, delegations, authors)

    /** Narrowed grants of this device for a prepared device; see `RockySession.delegate`. */
    suspend fun delegate(device: JsonObject, grants: List<Grant>, ttlSeconds: Long? = null, delegations: Int? = null, authors: Int? = null): JsonObject = session.delegate(device, GrantProfile.Grants(grants), ttlSeconds, delegations, authors)

    /** Replaces this device's credentials with a supplied bundle; see `RockySession.install`. */
    suspend fun install(supplied: JsonObject): SessionStatus = session.install(supplied)

    /** Bytes to carry to an assigner. The per-table secret key stays in this device's provider. */
    suspend fun pseudonymRequest(collection: String): String = session.client.call(request("pseudonym_request") { put("collection", collection) }).getValue("request").jsonPrimitive.content

    /** Assigns a stable author number and table grants; persists allocation before returning. */
    suspend fun assignPseudonym(requestBytes: String): JsonArray = session.client.call(request("pseudonym_assign") { put("request", requestBytes) }).getValue("pseudonym").jsonArray

    /** Verifies and stores the returned credential for this device's per-table signing key. */
    suspend fun installPseudonym(credential: JsonArray): AuthorPseudonym = session.client.call(request("pseudonym_install") { put("pseudonym", credential) }).let {
        AuthorPseudonym(it.getValue("collection").jsonPrimitive.content, it.getValue("author").jsonPrimitive.int)
    }

    /** Only an issuer can open the device binding sealed in a pseudonym credential. */
    suspend fun pseudonymOwner(credential: JsonArray): PseudonymOwner = session.client.call(request("pseudonym_owner") { put("pseudonym", credential) }).let {
        PseudonymOwner(it.getValue("node").jsonPrimitive.content, it.getValue("collection").jsonPrimitive.content, it.getValue("author").jsonPrimitive.int)
    }

    /** An issuer without a range claims one disjoint from every other issuer's range. */
    suspend fun claimAuthorRange(first: Int, last: Int) {
        session.client.call(
            request("pseudonym_range") {
                put("first", first)
                put("last", last)
            },
        )
    }
    suspend fun revoke(node: String) = session.revoke(node)
    suspend fun devices(): List<Device> = session.devices()
    suspend fun setDeviceLabel(node: String, label: ByteArray) = session.setDeviceLabel(node, label)
    suspend fun logout(purge: Boolean = false) = session.logout(purge)
}

@Suppress("REDUNDANT_ELSE_IN_WHEN")
private fun network(peers: PeerTransport): NativeNetwork = when (peers) {
    is PeerTransport.Custom -> NativeNetwork.foreign(ForeignAdapter(peers.transport))

    is PeerTransport.Endpoint -> NativeNetwork.endpoint(
        (
            peers.endpoint as? RockyEndpoint
                ?: throw RockyException("rejected", false, "the endpoint must be a RockyEndpoint")
            ).native,
    )

    else -> NativeNetwork.starter(
        buildJsonObject {
            when (peers) {
                is PeerTransport.Iroh -> {
                    put("kind", "iroh")
                    put("relay", peers.relay)
                    put("lookup", peers.lookup)
                    put("topic", peers.topic)
                    put("local", peers.local)
                    peers.bind?.let { put("bind", it) }
                }

                is PeerTransport.Tcp -> {
                    put("kind", "tcp")
                    peers.listen?.let { put("listen", it) }
                    peers.advertised?.let { put("advertised", it) }
                }

                else -> error("unsupported transport")
            }
        }.toString(),
    )
}
private suspend fun <T> nativeCall(call: suspend () -> T): T = try {
    call()
} catch (error: NativeException.Failure) {
    throw RockyException("storage", true, error.detail)
}

private class SessionTransport(private val native: NativeDatabaseSession, scope: String) : Transport {
    private val domain = Json.parseToJsonElement(scope)
    override suspend fun call(request: JsonObject): JsonObject = nativeCall {
        val response = Json.parseToJsonElement(
            native.call(
                buildJsonObject {
                    put("version", 1)
                    put("scope", domain)
                    put("request", request)
                }.toString(),
            ),
        ).jsonObject
        check(response.getValue("version").jsonPrimitive.int == 1)
        if (response.getValue("status").jsonPrimitive.content == "error") {
            val error = response.getValue("error").jsonObject
            throw RockyException(
                error.getValue("code").jsonPrimitive.content,
                error.getValue("retryable").jsonPrimitive.boolean,
                error.getValue("message").jsonPrimitive.content,
            )
        }
        response.getValue("value").jsonObject
    }
    override suspend fun close() = withContext(NonCancellable) {
        try {
            native.shutdown()
        } finally {
            native.destroy()
        }
    }
}

private inline fun <T> withNetwork(peers: PeerTransport?, call: (NativeNetwork?) -> T): T {
    val selected = peers?.let(::network)
    return try {
        call(selected)
    } finally {
        selected?.destroy()
    }
}
