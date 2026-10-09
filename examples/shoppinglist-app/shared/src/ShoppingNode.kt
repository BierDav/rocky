package org.example.examples.shopping

import androidx.room.RoomDatabase
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import rocky.CommandRuntime
import rocky.PeerTransport
import rocky.ReadinessState
import rocky.RockyDatabase
import rocky.SecretStore
import rocky.room.RockyRoom
import rocky.room.buildOnRocky
import java.io.File

/** What differs per platform: where data and keys live, how Room is built and how devices reach each other. */
class ShoppingPlatform(
    val directory: File,
    /** How this device names itself when it starts a household. */
    val deviceName: String,
    val room: (path: String) -> RoomDatabase.Builder<ShoppingDatabase>,
    val secrets: SecretStore? = null,
    /** Read on each open: iroh unless a driver chose another transport before the first one. */
    val transport: () -> PeerTransport = { PeerTransport.Iroh() },
    /** Read on each open, like [transport]: strong commands, such as [publicCoordination]; none disables approvals. */
    val commands: () -> CommandRuntime? = { null },
    /** Sees each opened database first, for instance to follow Android's app lifecycle. */
    val opened: (RockyDatabase) -> Unit = {},
)

/** One open installation: Rocky, Room on its file and the household's background work. Close it to release them. */
class ShoppingNode private constructor(
    val rocky: RockyDatabase,
    private val room: RockyRoom<ShoppingDatabase>,
    val self: String,
    commands: CommandRuntime?,
) {
    val repository = ShoppingRepository(room.room, rocky)
    val household = Household(room.room, rocky, self)
    val approvals = Approvals(room.room, rocky, household, self, commands != null)
    val isTrusted: Boolean get() = rocky.isIssuer
    private val work = CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
        launch { household.run() }
        launch { approvals.run() }
    }

    suspend fun close() = withContext(NonCancellable) {
        work.cancelAndJoin()
        try {
            room.close()
        } finally {
            rocky.close()
        }
    }

    companion object {
        /** A new household with this device as its first trusted device. */
        suspend fun create(platform: ShoppingPlatform): ShoppingNode = open(platform) { commands ->
            RockyDatabase.create(platform.directory, ShoppingSchema, platform.transport(), platform.secrets, commands = commands)
        }.configure {
            household.record(self, platform.deviceName, NodeKind.Trusted)
            Approvals.start(household.trusted)
        }

        /** Joins with a scanned [code]; a partial device holds only the lists shared with it. */
        suspend fun join(platform: ShoppingPlatform, code: PairingCode): ShoppingNode = open(platform) { commands ->
            RockyDatabase.join(platform.directory, ShoppingSchema, code.invitation, platform.transport(), platform.secrets, commands = commands)
        }.configure {
            rocky.partitions.role(code.kind.role)
            if (isTrusted) household.record(self, code.name, NodeKind.Trusted)
        }

        suspend fun resume(platform: ShoppingPlatform): ShoppingNode = open(platform) { commands ->
            RockyDatabase.resume(platform.directory, ShoppingSchema, platform.transport(), platform.secrets, commands = commands)
        }.configure {
            if (isTrusted && rocky.observeReadiness().first().state != ReadinessState.Revoked && household.devices().first().none { it.node == self }) {
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
                    try {
                        platform.opened(rocky)
                        val self = rocky.session().node
                        // Room keeps only a lock file next to Rocky's.
                        val room = platform.room(File(platform.directory, "shopping").absolutePath).buildOnRocky(rocky)
                        ShoppingNode(rocky, room, self, commands).also { opened = it }
                    } catch (error: Throwable) {
                        withContext(NonCancellable) { rocky.close() }
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
