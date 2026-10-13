package org.example.examples.shopping.data.session

import androidx.room.RoomDatabase
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.ShoppingRepository
import org.example.examples.shopping.data.coordination.Approvals
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.ShoppingSchema
import org.example.examples.shopping.data.household.Household
import org.example.examples.shopping.data.household.NodeKind
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.util.readText
import org.example.examples.shopping.util.replaceText
import rocky.CommandRuntime
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.SecretStore
import rocky.room.RockyRoom
import rocky.room.buildOnRocky

/** What differs per platform: where data and keys live, how Room is built and how devices reach each other. */
class ShoppingPlatform(
    val directory: String,
    /** How this device names itself when it starts a household. */
    val deviceName: String,
    val room: (path: String) -> RoomDatabase.Builder<ShoppingDatabase>,
    /** Read on each open, so a reset can choose fresh keys. */
    val secrets: () -> SecretStore? = { null },
    /** Read on each open: iroh unless a driver chose another transport before the first one. */
    val transport: () -> PeerTransport = { PeerTransport.Iroh() },
    /** Read on each open, like [transport]: strong commands, such as [publicCoordination]; none disables approvals. */
    val commands: () -> CommandRuntime? = { null },
    /** Sees each opened database first, for instance to follow Android's app lifecycle. */
    val opened: (RockyDatabase) -> Unit = {},
    /** Sees each database before it closes, undoing [opened]. */
    val closed: (RockyDatabase) -> Unit = {},
    /** On reset, after [directory] is removed: forgets what else the installation kept, such as keys and settings. */
    val erase: suspend () -> Unit = {},
    /** Platform visibility cancels application upkeep alongside SDK sync. */
    val foreground: Flow<Boolean> = flowOf(true),
)

/** One open installation: Rocky, Room on its file and the household's background work. Close it to release them. */
class ShoppingNode private constructor(
    val rocky: RockyDatabase,
    private val room: RockyRoom<ShoppingDatabase>,
    val self: String,
    commands: CommandRuntime?,
    foreground: Flow<Boolean>,
    private val kindPath: String,
    initialKind: NodeKind?,
    private val closed: (RockyDatabase) -> Unit,
    internal val failure: Deferred<Throwable>,
    private val backgroundErrors: CoroutineExceptionHandler,
) {
    /** Flows the household shares between collectors; cancelled before Rocky closes. */
    private val shared = CoroutineScope(SupervisorJob() + Dispatchers.IO + backgroundErrors)
    val repository = ShoppingRepository(room.room, rocky)
    val household = Household(room.room, rocky, self, shared)
    val approvals = Approvals(room.room, rocky, household, self, commands != null)
    val isTrusted: Boolean get() = rocky.isIssuer
    var kind: NodeKind? = initialKind
        private set

    private suspend fun rememberKind(kind: NodeKind) {
        replaceText(kindPath, kind.name)
        this.kind = kind
    }
    private val work = CoroutineScope(SupervisorJob() + Dispatchers.IO + backgroundErrors).launch {
        foreground.distinctUntilChanged().collectLatest { visible ->
            if (visible) {
                coroutineScope {
                    launch { household.run() }
                    launch { approvals.run() }
                }
            }
        }
    }

    suspend fun close() = withContext(NonCancellable) {
        work.cancelAndJoin()
        shared.coroutineContext.job.cancelAndJoin()
        try {
            closed(rocky)
            room.close()
        } finally {
            rocky.close()
        }
    }

    companion object {
        /** A new household with this device as its first trusted device. */
        suspend fun create(platform: ShoppingPlatform): ShoppingNode = open(platform) { commands ->
            RockyDatabase.create(platform.directory, ShoppingSchema, platform.transport(), platform.secrets(), commands = commands)
        }.configure {
            rememberKind(NodeKind.Trusted)
            household.record(self, platform.deviceName, NodeKind.Trusted)
            Approvals.start(household.trusted)
        }

        /** Joins with a scanned [code]; a partial device holds only the lists shared with it. */
        suspend fun join(platform: ShoppingPlatform, code: PairingCode): ShoppingNode = open(platform) { commands ->
            RockyDatabase.join(platform.directory, ShoppingSchema, code.invitation, platform.transport(), platform.secrets(), commands = commands)
        }.configure {
            rememberKind(code.kind)
            rocky.partitions.role(code.kind.role)
            if (isTrusted) household.record(self, code.name, NodeKind.Trusted)
        }

        suspend fun resume(platform: ShoppingPlatform): ShoppingNode = open(platform) { commands ->
            RockyDatabase.resume(platform.directory, ShoppingSchema, platform.transport(), platform.secrets(), commands = commands)
        }.configure {
            if (isTrusted && rocky.observeStatus().first().authority.verifiedRevocation == null && household.devices().first().none { it.node == self }) {
                household.record(self, platform.deviceName, NodeKind.Trusted)
            }
        }

        private suspend fun ShoppingNode.configure(block: suspend ShoppingNode.() -> Unit): ShoppingNode = try {
            block()
            this
        } catch (error: Throwable) {
            close()
            throw error
        }

        private suspend fun open(platform: ShoppingPlatform, open: suspend (CommandRuntime?) -> RockyDatabase): ShoppingNode {
            var opened: ShoppingNode? = null
            try {
                return withContext(Dispatchers.IO) {
                    val commands = platform.commands()
                    val rocky = open(commands)
                    var room: RockyRoom<ShoppingDatabase>? = null
                    try {
                        platform.opened(rocky)
                        val self = rocky.session().node
                        // Room keeps only a lock file next to Rocky's.
                        val failure = CompletableDeferred<Throwable>()
                        val backgroundErrors = CoroutineExceptionHandler { _, error -> failure.complete(error) }
                        val openedRoom = platform.room("${platform.directory}/shopping").buildOnRocky(rocky, Dispatchers.IO + backgroundErrors)
                        room = openedRoom
                        val kindPath = "${platform.directory}/device-kind"
                        val kind = readText(kindPath)?.let { requireNotNull(NodeKind.of(it)) { "Unknown local device kind" } }
                        ShoppingNode(rocky, openedRoom, self, commands, platform.foreground, kindPath, kind, platform.closed, failure, backgroundErrors).also { opened = it }
                    } catch (error: Throwable) {
                        withContext(NonCancellable) {
                            try {
                                platform.closed(rocky)
                                room?.close()
                            } finally {
                                rocky.close()
                            }
                        }
                        throw error
                    }
                }
            } catch (error: Throwable) {
                // Cancellation can discard the result when returning from the IO dispatcher.
                opened?.close()
                throw error
            }
        }
    }
}
