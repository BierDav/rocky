package org.example.examples.shopping

import java.io.File
import kotlinx.coroutines.*
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.SecretStore
import rocky.room.followRocky

/** Owns the background jobs; close before exiting the desktop process. */
class ShoppingDesktop private constructor(val runtime: RockyDatabase, val database: ShoppingDatabase) {
    val repository = ShoppingRepository(database)
    private val lifetime = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    // Sync writes reach Room's observers only through this.
    private val invalidation = lifetime.launch { database.followRocky(runtime) }
    suspend fun close() = withContext(NonCancellable) {
        lifetime.cancel()
        invalidation.join()
        try { database.close() } finally { runtime.close() }
    }
    companion object {
        /** A new device that issues assigns its own author; another one asks an issuer, see [installShoppingAuthor]. */
        private suspend fun compose(directory: File, runtime: RockyDatabase, new: Boolean = false): ShoppingDesktop {
            try {
                if (new && runtime.isIssuer) runtime.installShoppingAuthor()
                return ShoppingDesktop(runtime, openShoppingDatabase(directory, runtime))
            }
            catch (error: Throwable) { withContext(NonCancellable) { runtime.close() }; throw error }
        }
        /** Devices reach each other through iroh unless another transport is selected. */
        suspend fun create(directory: File, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null): ShoppingDesktop =
            compose(directory, RockyDatabase.create(directory, ShoppingSchema, transport, secrets), new = true)
        suspend fun join(directory: File, invitation: String, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null): ShoppingDesktop =
            compose(directory, RockyDatabase.join(directory, ShoppingSchema, invitation, transport, secrets), new = true)
        suspend fun resume(directory: File, transport: PeerTransport = PeerTransport.Iroh(), secrets: SecretStore? = null): ShoppingDesktop =
            compose(directory, RockyDatabase.resume(directory, ShoppingSchema, transport, secrets))
    }
}
