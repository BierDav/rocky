package org.example.examples.shopping.data.session

import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.data.household.PairingCode
import org.example.examples.shopping.util.epochMillis
import org.example.examples.shopping.util.moveDirectory
import org.example.examples.shopping.util.removeDirectory
import rocky.ReadinessState
import rocky.RockyDatabase

/** Where the app stands: setting up, opening, open, revoked, resetting, or failed. */
sealed interface SessionState {
    data object Opening : SessionState

    /** Closing and removing this device's household; setup follows. */
    data object Resetting : SessionState

    /** No household on this device yet: start one or join one. */
    data object Setup : SessionState
    data class Open(val node: ShoppingNode) : SessionState
    data class Revoked(val node: ShoppingNode, val error: Throwable? = null) : SessionState
    data class Failed(val error: Throwable, val existing: Boolean) : SessionState
}

/** The app's one installation, opened at start when it exists. */
@SingleIn(AppScope::class)
@Inject
class ShoppingSession(private val platform: ShoppingPlatform) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val mutableState = MutableStateFlow<SessionState>(SessionState.Opening)
    val state: StateFlow<SessionState> = mutableState.asStateFlow()

    /** The open installation, whenever there is one. */
    val node: Flow<ShoppingNode> = state.mapNotNull { (it as? SessionState.Open)?.node }.distinctUntilChanged()
    private var lastOpen: suspend () -> ShoppingNode = { ShoppingNode.resume(platform) }

    init {
        if (RockyDatabase.exists(platform.directory)) start { ShoppingNode.resume(platform) } else mutableState.value = SessionState.Setup
    }

    fun create() = start { ShoppingNode.create(platform) }

    /** Joins with a scanned or pasted pairing code. */
    fun join(code: String) = start { ShoppingNode.join(platform, PairingCode.parse(code)) }

    /** Keeps the revoked copy, then enrolls a fresh identity with a new code. */
    fun rejoin(code: String) {
        val revoked = state.value as? SessionState.Revoked ?: return
        val pairing = try {
            PairingCode.parse(code)
        } catch (error: Exception) {
            mutableState.value = revoked.copy(error = error)
            return
        }
        start {
            revoked.node.close()
            val archive = "${platform.directory}-revoked-${revoked.node.self.take(12)}-${epochMillis()}"
            check(moveDirectory(platform.directory, archive)) { "Could not save the revoked household copy" }
            lastOpen = { ShoppingNode.join(platform, pairing) }
            lastOpen()
        }
    }

    /** Joins, or rejoins a revoked device, with a pairing link's code once the user confirmed it. */
    fun accept(code: PairingCode) {
        when (state.value) {
            SessionState.Setup -> join(code.toString())
            is SessionState.Revoked -> rejoin(code.toString())
            else -> {}
        }
    }

    /** Retries an existing installation, or the same interrupted setup. */
    fun retry() {
        if (state.value !is SessionState.Failed) return
        if (RockyDatabase.exists(platform.directory)) start { ShoppingNode.resume(platform) } else start(lastOpen)
    }

    /** After a failed setup: removes what it left, so another code can be used. */
    fun startOver() {
        if (state.value !is SessionState.Failed || RockyDatabase.exists(platform.directory)) return
        scope.launch {
            if (state.value !is SessionState.Failed || RockyDatabase.exists(platform.directory)) return@launch
            removeDirectory(platform.directory)
            mutableState.value = SessionState.Setup
        }
    }

    private fun start(open: suspend () -> ShoppingNode) {
        lastOpen = open
        mutableState.value = SessionState.Opening
        scope.launch {
            mutableState.value = try {
                SessionState.Open(open())
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                SessionState.Failed(error, RockyDatabase.exists(platform.directory))
            }
            val node = (state.value as? SessionState.Open)?.node ?: return@launch
            val failure = try {
                merge(
                    flow<Throwable?> { emit(node.failure.await()) },
                    node.rocky.observeReadiness().filter { it.state == ReadinessState.Revoked }.map { null },
                ).first()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                error
            }
            if ((state.value as? SessionState.Open)?.node !== node) return@launch
            if (failure == null) {
                mutableState.value = SessionState.Revoked(node)
            } else {
                // Leave Open before closing so screens stop using the failed installation.
                mutableState.value = SessionState.Opening
                try {
                    node.close()
                } catch (closing: Exception) {
                    failure.addSuppressed(closing)
                }
                mutableState.value = SessionState.Failed(failure, RockyDatabase.exists(platform.directory))
            }
        }
    }

    /**
     * Closes this installation and removes its local data with the platform's keys and settings, then offers setup
     * again. Other devices keep their copies. A failure leaves [SessionState.Failed].
     */
    fun reset() {
        val current = state.value
        if (current == SessionState.Opening || current == SessionState.Resetting) return
        mutableState.value = SessionState.Resetting
        scope.launch {
            val self = coroutineContext.job
            scope.coroutineContext.job.children.filter { it != self }.toList().forEach { it.cancelAndJoin() }
            mutableState.value = try {
                closeNode(current)
                removeDirectory(platform.directory)
                platform.erase()
                lastOpen = { ShoppingNode.resume(platform) }
                SessionState.Setup
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                SessionState.Failed(error, RockyDatabase.exists(platform.directory))
            }
        }
    }

    suspend fun close() = withContext(NonCancellable) {
        scope.coroutineContext.job.cancelAndJoin()
        closeNode(state.value)
    }

    private suspend fun closeNode(current: SessionState) = when (current) {
        is SessionState.Open -> current.node.close()
        is SessionState.Revoked -> current.node.close()
        else -> {}
    }
}
