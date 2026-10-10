package org.example.examples.market

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.serialization.json.*
import rocky.*
import rocky.room.settledOnRocky

/** A memo with this key prefix carries a device's command identity for the owner; see [Market.publishIdentity]. */
internal const val IDENTITY = "identity:"

/** The domain's control as the owner last had it accepted, or is about to retry with the same [id]. */
internal data class Control(val revision: Long, val id: String, val executors: List<String>, val invokers: List<String>) {
    fun encoded(): ByteArray = buildJsonObject {
        put("revision", revision)
        put("id", id)
        put("executors", JsonArray(executors.map(::JsonPrimitive)))
        put("invokers", JsonArray(invokers.map(::JsonPrimitive)))
    }.toString().encodeToByteArray()

    companion object {
        fun decode(bytes: ByteArray): Control = Json.parseToJsonElement(bytes.decodeToString()).jsonObject.let { value ->
            fun list(name: String) = value.getValue(name).jsonArray.map { it.jsonPrimitive.content }
            Control(value.getValue("revision").jsonPrimitive.long, value.getValue("id").jsonPrimitive.content, list("executors"), list("invokers"))
        }
    }
}

/**
 * The market on one device: mergeable listings and memos through Room, stock and holds through [Reserve] and
 * [Restock]. The coordination service orders both commands, so of two reservations for the last item one is accepted
 * and the other rejected on every device. The owner, the device that created the market, keeps the domain's control.
 */
class Market internal constructor(
    val rocky: RockyDatabase,
    val room: MarketDatabase,
    private val secrets: SecretStore,
    private val secureRandom: (Int) -> ByteArray,
) {
    /** Listings with their confirmed stock. */
    fun offers(): Flow<List<Offer>> = room.listings().offers().settledOnRocky()

    /** Confirmed holds. */
    fun holds(): Flow<List<Hold>> = room.stock().holds().settledOnRocky()

    /** Buyers' notes, without the identities devices publish. */
    fun memos(): Flow<List<Memo>> = room.memos().observe().settledOnRocky().map { memos -> memos.filterNot { it.key.startsWith(IDENTITY) } }

    suspend fun addListing(key: String, title: String) = room.listings().add(key, title)

    /** A mergeable note: written and synced without the coordination service. Returns its key. */
    suspend fun addMemo(listing: String, note: String): String = RockyDatabase.newHandle().also { room.memos().add(it, listing, note) }

    /** Submits the reservation under [handle] and runs it once; the ticket stays stored until it settles. */
    suspend fun reserve(item: String, quantity: Long, handle: String = RockyDatabase.newHandle(), hold: String = handle): Ticket<Remaining, Shortfall> {
        rocky.submit(Reserve, ReserveArguments(item, quantity, hold), handle)
        return rocky.run(Reserve, handle)
    }

    suspend fun restock(item: String, quantity: Long, handle: String = RockyDatabase.newHandle()): Ticket<Remaining, Unit> {
        rocky.submit(Restock, RestockArguments(item, quantity), handle)
        return rocky.run(Restock, handle)
    }

    suspend fun reservation(handle: String): Ticket<Remaining, Shortfall>? = rocky.ticket(Reserve, handle)

    /** Runs a stored reservation again: after any uncertainty the same ticket, never a new one. */
    suspend fun retry(handle: String): Ticket<Remaining, Shortfall> = rocky.run(Reserve, handle)

    /** Publishes this device's command identity, so the owner admits it as an invoker. */
    suspend fun publishIdentity(): CommandIdentity {
        val identity = rocky.commandIdentity()
        val key = IDENTITY + identity.node
        if (room.memos().observe().settledOnRocky().first().none { it.key == key }) room.memos().add(key, "", identity.encoded)
        return identity
    }

    /** On the owner: the identities devices published, its own first. */
    suspend fun published(): List<CommandIdentity> {
        val self = rocky.commandIdentity()
        val others = room.memos().observe().settledOnRocky().first()
            .filter { it.key.startsWith(IDENTITY) }
            .mapNotNull { runCatching { CommandIdentity.decode(it.note) }.getOrNull() }
            .filter { it.node != self.node }
        return listOf(self) + others.sortedBy { it.node }
    }

    /** Whether this device created the market and so holds the domain's tenant key. */
    val isOwner: Boolean get() = secrets.load(TENANT) != null

    /**
     * The app's upkeep until cancelled: publishes this device's identity, on the owner admits published identities,
     * runs unresolved tickets again and lets the service retire settled ones. Each failure waits for the next round.
     */
    suspend fun maintain(periodMillis: Long = 5_000) {
        while (true) {
            runCatching {
                publishIdentity()
                if (isOwner) control()
                rocky.runCommands()
                rocky.acknowledgeCommands()
            }
            delay(periodMillis)
        }
    }

    /** The tenant key of the owner, created once. Only the device that created the market has one. */
    private fun tenant(): ByteArray = secrets.load(TENANT) ?: secureRandom(32).also { secrets.store(TENANT, it) }
    private fun owner() = rocky.service(tenant())
    private fun stored(): Control? = secrets.load(CONTROL)?.let(Control::decode)

    /**
     * On the owner: registers the domain, or moves its control to [executors] (by default the current ones, at first
     * this device) and [invokers] (by default every published identity). Each change is stored
     * before it is sent and retried with the same identity, so a lost answer never registers twice. Moving the
     * executors fences the current one; another continues at the same head on its next run.
     */
    suspend fun control(executors: List<CommandIdentity>? = null, invokers: List<CommandIdentity>? = null): ServiceControl {
        val current = stored()
        val executors = executors ?: current?.executors?.map(CommandIdentity::decode) ?: listOf(rocky.commandIdentity())
        val invokers = invokers ?: published()
        val wanted = Control(0, "", executors.map { it.encoded }, invokers.map { it.encoded })
        val next = when {
            current == null -> wanted.copy(revision = 1, id = RockyDatabase.newHandle())
            current.executors == wanted.executors && current.invokers == wanted.invokers -> current
            else -> wanted.copy(revision = current.revision + 1, id = RockyDatabase.newHandle())
        }
        if (next != current) secrets.store(CONTROL, next.encoded())
        val decode = { list: List<String> -> list.map(CommandIdentity::decode) }
        return if (next.revision == 1L) {
            owner().register(next.id, decode(next.executors), decode(next.invokers))
        } else {
            owner().control(next.revision, next.id, decode(next.executors), decode(next.invokers))
        }
    }

    /** On the owner: closes the domain at its head for good, freeing the tenant's open-domain quota. */
    suspend fun closeDomain() {
        checkNotNull(stored()) { "this device registered no domain" }
        val id = secrets.load(CLOSE)?.decodeToString() ?: RockyDatabase.newHandle().also { secrets.store(CLOSE, it.encodeToByteArray()) }
        owner().close(id)
    }

    companion object {
        /**
         * A buyer reads listings, writes memos and invokes [Reserve]; it cannot restock or execute. A command's row
         * effects must lie within its caller's rights, so a buyer holds insert and update on `stock` and `holds`;
         * SQL still refuses its direct writes there, as on every command-controlled table.
         */
        val Buyer = GrantProfile.Grants(
            listOf(
                Grant("listings", setOf(Right.Read)),
                Grant("memos", setOf(Right.Read, Right.Insert, Right.Update, Right.Delete)),
                Grant("stock", setOf(Right.Read, Right.Insert, Right.Update)),
                Grant("holds", setOf(Right.Read, Right.Insert, Right.Update, Right.Invoke)),
            ),
        )

        private const val TENANT = "market.tenant"
        private const val CONTROL = "market.control"
        private const val CLOSE = "market.close"
    }
}
