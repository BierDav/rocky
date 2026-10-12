package rocky

import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.json.JsonObject

/** A write is durably committed before this port acknowledges it. */
interface RuntimePort {
    val domain: String
    fun observe(query: JsonObject): Flow<JsonObject>
    suspend fun read(query: JsonObject): JsonObject

    /**
     * Runs `block` as one SQL transaction committed under `handle`; a failure rolls it back. Persist `handle` first;
     * after uncertainty, reconcile it instead of writing again: another transaction under a committed handle is refused.
     * Inserts need this device's author for the table.
     */
    suspend fun <T> write(handle: String = newMutationHandle(), block: suspend SqlWrite.() -> T): T
}

/** Statements of one open transaction. Values are `null`, `Boolean`, `Int`, `Long`, `Double`, `String` or `ByteArray`. */
interface SqlWrite {
    /** A Rocky-issued `id` for a new row of `table`, signed under this device's author. */
    suspend fun nextId(table: String): Long

    /** Partition values follow partitionBy order; the adapter must implement this boundary. */
    suspend fun nextId(table: String, vararg partitionValues: Any?): Long {
        require(partitionValues.isEmpty()) { "this runtime port does not support partitioned ids" }
        return nextId(table)
    }

    /** Runs one statement and returns how many rows it changed. */
    suspend fun execute(sql: String, vararg values: Any?): Long
}

/** A single-use `rocky://join/…` URI. It carries the issuer's routes, the invitation secret and the profile it grants. */
data class PairingInvitation(val uri: String, val expiresAtEpochSeconds: Long, val profile: GrantProfile, val sequence: Long)

/** A right a raw grant names on one table. `Invoke` and `Execute` need a command that uses the table. */
enum class Right(val wire: String) {
    Read("read"),
    Insert("insert"),
    Update("update"),
    Delete("delete"),
    Invoke("invoke"),
    Execute("execute"),
    Custody("custody"),
    ;

    companion object {
        fun of(wire: String): Right = entries.firstOrNull { it.wire == wire } ?: throw IllegalStateException("unknown right $wire")
    }
}

/** Rights over one table, limited to `columns` when given. Any set is issuable, `Execute` and `Custody` included. */
data class Grant(val table: String, val rights: Set<Right>, val columns: Set<String>? = null)

/**
 * What a device may do. There is no default: the issuer names one. `Grants` is the primitive;
 * reader, writer and issuer are the optional starter layer built on it.
 */
sealed interface GrantProfile {
    /** Holds encrypted frames of the named tables without reader keys. */
    data class Custody(val tables: Set<String>) : GrantProfile

    /** Reads, and holds the keys of, the named tables. */
    data class Reader(val tables: Set<String>) : GrantProfile

    /** Reads and writes the named tables. */
    data class Writer(val tables: Set<String>) : GrantProfile

    /** Writes every table and holds the domain's publisher key: it invites, renews and revokes devices, other issuers included. */
    object Issuer : GrantProfile

    /** Exactly these raw grants. */
    data class Grants(val grants: List<Grant>) : GrantProfile
}

/**
 * How devices reach each other. Sessions are authenticated and encrypted above every transport,
 * and a domain is bound to none: a device may come back on another one and learn new routes.
 */
sealed interface PeerTransport {
    /**
     * The starter's own iroh endpoint. `relay` is `default`, `staging`, `disabled` or a relay URL. `lookup` is where
     * endpoint addresses are published and resolved: `default` (n0's DNS service), `disabled` or a pkarr relay URL.
     * `topic` finds domain members through a rendezvous record on the lookup service, `local` on the local network
     * over mDNS. Disabling `local` also disables multicast gateway discovery.
     * See `docs/runtime/sdk-transports.md#discovery`.
     */
    data class Iroh(
        val relay: String = "default",
        val lookup: String = "default",
        val topic: Boolean = true,
        val local: Boolean = true,
        val bind: String? = null,
    ) : PeerTransport {
        companion object {
            /** No relay, lookup service or discovery: peers are reached at their contacts' addresses only. */
            fun isolated(bind: String = "127.0.0.1:0") = Iroh("disabled", "disabled", topic = false, local = false, bind = bind)
        }
    }

    /** An endpoint the application owns, which also serves its own protocols. Closing the database leaves it open. */
    class Endpoint(val endpoint: AppEndpoint) : PeerTransport
}

/** An application-owned endpoint backed by `RockyEndpoint`. */
interface AppEndpoint

/** One reliable ordered byte stream, as chunks. Implementations must not fail `close`. */
interface ByteStream {
    /** The next bytes, at most `max`. Empty once the peer finished. */
    suspend fun read(max: Int): ByteArray

    /** Delivers `bytes` in order; returns once they are accepted. */
    suspend fun write(bytes: ByteArray)

    /** No more writes follow: the peer's `read` ends after what was sent. */
    suspend fun finish()

    /** Abandons the stream; a pending `read` ends. */
    suspend fun close()
}

/** One way to reach a device. A hint: whoever answers must still prove the device's key. */
data class Route(val transport: String, val address: String)

/**
 * A peer met or kept as a contact: its source, last selection reason, issuer/contact flags, routes and what
 * the last sync with it did: `connected`, `unreachable` or `unknown`. A peer met only through its own session has no routes.
 */
data class Peer(
    val node: String,
    val isIssuer: Boolean,
    val isConfigured: Boolean,
    val source: String,
    val choice: String,
    val routes: List<Route>,
    /** Found on the local network by discovery; such peers are tried first. */
    val isLocal: Boolean = false,
    val state: String,
    val checkedAtEpochSeconds: Long?,
    val error: String?,
    val link: PeerLink,
)

enum class LinkState { Open, Opening, Closed }

/**
 * Link lifetime and its network paths, measured by the transport when status is read. [roundTripMillis] is the
 * selected path's. Measurements alone do not emit readiness changes.
 */
data class PeerLink(
    val state: LinkState,
    val openedAtEpochSeconds: Long?,
    val roundTripMillis: Long?,
    val paths: List<LinkPath> = emptyList(),
)

/**
 * One network path of an open link, the selected one first. [kind] is `direct` for an IP path or `relay`; [remote]
 * is the peer's socket address or relay URL. Counters cover the path's lifetime; [window] is the congestion window.
 */
data class LinkPath(
    val selected: Boolean,
    val kind: String,
    val remote: String,
    val local: String?,
    val roundTripMicros: Long,
    val mtu: Int,
    val window: Long,
    val sentBytes: Long,
    val sentDatagrams: Long,
    val receivedBytes: Long,
    val receivedDatagrams: Long,
    val lostPackets: Long,
    val lostBytes: Long,
    val congestionEvents: Long,
)

/** A `rocky://peer/…` URI naming this device and its current route. It grants nothing. */
data class Contact(val uri: String, val node: String, val routes: List<Route>)

enum class DiscoveryPathState { On, Off, Unavailable }

/** One way of finding peers: whether it runs and, when it cannot, why. */
data class DiscoveryPath(val state: DiscoveryPathState, val reason: String? = null)

/**
 * How this device finds domain members it has no contact for: on the [local] network and through the [topic]'s
 * rendezvous record. [announcing] is false until an admitted session handed over the domain's discovery root.
 */
data class DiscoveryStatus(val announcing: Boolean, val local: DiscoveryPath, val topic: DiscoveryPath)

/** The network service, reported whatever the state of the database. */
data class NetworkStatus(
    val attached: Boolean,
    val kind: String?,
    val owned: Boolean,
    val inboundActive: Int,
    val inboundLimit: Int,
    val inboundServed: Long,
    val inboundRefused: Long,
    val peers: List<Peer>,
    val discovery: DiscoveryStatus,
)

/** Platform storage for an installation's keys, such as a keychain. Names use only `[a-z0-9._-]`. */
interface SecretStore {
    fun load(name: String): ByteArray?
    fun store(name: String, secret: ByteArray)
    fun delete(name: String)
}

/** The revocations a device holds: the lowercase hex [digest] of the live set, which two devices share when they hold the same statements, and how many [statements] are live. */
data class RevocationStatus(val digest: String, val statements: Int)

/** The authority this device holds. Keys are lowercase hex; `expiresAtEpochSeconds` ends its credentials. `transport` is the attached network's kind. */
data class SessionStatus(val principal: String, val node: String, val revocations: RevocationStatus, val expiresAtEpochSeconds: Long, val isIssuer: Boolean, val profile: GrantProfile, val transport: String)

/**
 * Another device's record in the replicated issuer registry, which every issuer reads: the profile its grants amount
 * to (empty grants when only partition grants are known), when its latest chain expires, its command identity and an
 * opaque application [label] of at most 256 bytes.
 */
data class Device(
    val node: String,
    val user: String,
    val enrolledAtEpochSeconds: Long,
    val revoked: Boolean,
    val profile: GrantProfile,
    val expiresAtEpochSeconds: Long? = null,
    val command: String? = null,
    val label: ByteArray? = null,
)

enum class ConnectionState { Connected, Offline, Unavailable, Revoked }

/** `Syncing` while a sync pass runs, `Bootstrapping` while a scope restarts from a checkpoint. */
enum class ReadinessState { Ready, Syncing, Bootstrapping, Unavailable, Revoked }

/** The database's committed [revision], its state and the authority it acts under. */
data class Readiness(
    val state: ReadinessState,
    val revision: Long,
    val revocations: RevocationStatus,
    val expiresAtEpochSeconds: Long,
    val profile: GrantProfile,
    val networkAttached: Boolean,
    /** False on a member that can reach no issuer to renew its credentials. */
    val issuerContact: Boolean = true,
    val discovery: DiscoveryStatus = DiscoveryStatus(false, DiscoveryPath(DiscoveryPathState.Off), DiscoveryPath(DiscoveryPathState.Off)),
    val peers: List<Peer> = emptyList(),
    /** How far a checkpoint bootstrap has imported; null until its peer's offers are known. */
    val bootstrap: BootstrapProgress? = null,
)

/** Rows imported of all offered checkpoints of a bootstrap, and whole scopes done. */
data class BootstrapProgress(val rowsDone: Long, val rowsTotal: Long, val scopesDone: Int, val scopesTotal: Int) {
    /** Whole percent imported, 100 for a bootstrap with no rows. */
    val percent: Int get() = if (rowsTotal == 0L) 100 else (rowsDone * 100 / rowsTotal).toInt()
}

/** The last sync: its state, its error, how many [peers] it reached and when it ended. */
data class ConnectionStatus(val state: ConnectionState, val error: String? = null, val peers: Int = 0, val atEpochMillis: Long? = null)

/** Unique operation identity, not a credential. Persist it with the queued action. */
fun newMutationHandle(): String = kotlin.random.Random.Default.nextBytes(32)
    .joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }
