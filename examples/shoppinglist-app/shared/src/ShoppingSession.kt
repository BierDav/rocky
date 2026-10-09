package org.example.examples.shopping

import java.io.File
import dev.zacsweers.metro.AppScope
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import rocky.RockyDatabase
import rocky.ReadinessState

/** Where the app stands: setting up, opening, open, or failed to open. */
sealed interface SessionState {
    data object Opening : SessionState
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
    private val mutableOffer = MutableStateFlow<PairingCode?>(null)
    /** A pairing code that arrived as a link, until the user joins with it or dismisses it. */
    val offer: StateFlow<PairingCode?> = mutableOffer.asStateFlow()

    init {
        if (RockyDatabase.exists(platform.directory)) start { ShoppingNode.resume(platform) } else mutableState.value = SessionState.Setup
    }

    fun create() = start { ShoppingNode.create(platform) }
    /** Joins with a scanned or pasted pairing code. */
    fun join(code: String) = start { ShoppingNode.join(platform, PairingCode.parse(code)) }
    /** Keeps the revoked copy, then enrolls a fresh identity with a new code. */
    fun rejoin(code: String) {
        val revoked = state.value as? SessionState.Revoked ?: return
        val pairing = try { PairingCode.parse(code) }
        catch (error: Exception) { mutableState.value = revoked.copy(error = error); return }
        start {
            revoked.node.close()
            val archive = File(platform.directory.parentFile, "${platform.directory.name}-revoked-${revoked.node.self.take(12)}-${System.currentTimeMillis()}")
            check(platform.directory.renameTo(archive)) { "Could not save the revoked household copy" }
            lastOpen = { ShoppingNode.join(platform, pairing) }
            lastOpen()
        }
    }
    /** Shows a pairing link for confirmation: any app or page may open one, so it never joins on its own. */
    fun offer(code: PairingCode) { mutableOffer.value = code }
    fun dismissOffer() { mutableOffer.value = null }
    /** Joins, or rejoins a revoked device, with the offered code. */
    fun acceptOffer() {
        val code = mutableOffer.value ?: return
        mutableOffer.value = null
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
        platform.directory.deleteRecursively()
        mutableState.value = SessionState.Setup
    }

    private fun start(open: suspend () -> ShoppingNode) {
        lastOpen = open
        mutableState.value = SessionState.Opening
        scope.launch {
            mutableState.value = try { SessionState.Open(open()) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) { SessionState.Failed(error, RockyDatabase.exists(platform.directory)) }
            val node = (state.value as? SessionState.Open)?.node ?: return@launch
            node.rocky.observeReadiness().first { it.state == ReadinessState.Revoked }
            if ((state.value as? SessionState.Open)?.node === node) mutableState.value = SessionState.Revoked(node)
        }
    }

    suspend fun close() = withContext(NonCancellable) {
        scope.coroutineContext.job.cancelAndJoin()
        when (val current = state.value) {
            is SessionState.Open -> current.node.close()
            is SessionState.Revoked -> current.node.close()
            else -> {}
        }
    }
}
