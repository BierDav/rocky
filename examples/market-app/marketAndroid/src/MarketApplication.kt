package org.example.examples.market

import android.app.Application
import android.content.Intent
import android.util.Base64
import androidx.room.Room
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.android.KeystoreSecretStore
import rocky.android.RockyLifecycle
import java.io.File
import java.security.SecureRandom

/** Where the app stands: opening, open, or why it is not. */
sealed interface AppState {
    data object Opening : AppState
    data class Waiting(val message: String) : AppState
    data class Open(val node: MarketNode) : AppState
}

/**
 * Keeps the market open for the process: secrets in the Keystore, sync paused while no activity is visible. The first
 * launch is driven over adb with the service's bundle and `create` or `join`; see `examples/market-app/README.md`.
 */
class MarketApplication : Application() {
    private val state = MutableStateFlow<AppState>(AppState.Opening)
    val current: StateFlow<AppState> = state.asStateFlow()
    private lateinit var lifecycle: RockyLifecycle
    private val opening = Mutex()
    private val directory get() = File(filesDir, "market")
    private val bundle get() = File(filesDir, "bundle.json")
    private val loopback get() = File(filesDir, "loopback")

    override fun onCreate() {
        super.onCreate()
        lifecycle = RockyLifecycle.register(this)
    }

    /** Common setup screen: the same service bundle and opening choices as the iOS app. */
    suspend fun openFromUi(bundle: String, opening: Opening) {
        val intent = Intent().putExtra("bundle", Base64.encodeToString(bundle.toByteArray(), Base64.NO_WRAP))
        when (opening) {
            Opening.Create -> intent.putExtra("create", true)
            is Opening.Join -> intent.putExtra("join", opening.invitation).putExtra("direct-address", opening.directAddress)
            Opening.Resume -> {}
        }
        open(intent)
    }

    /** Opens once per process: the existing market, or the one the launching [intent] names. */
    suspend fun open(intent: Intent) = opening.withLock { openOnce(intent) }

    private suspend fun openOnce(intent: Intent) {
        if (state.value is AppState.Open) return
        intent.getStringExtra("bundle")?.let { bundle.writeBytes(Base64.decode(it, Base64.DEFAULT)) }
        if (intent.hasExtra("loopback")) loopback.writeText("1")
        val opening = when {
            RockyDatabase.exists(File(directory, "rocky")) -> Opening.Resume
            intent.getBooleanExtra("create", false) -> Opening.Create
            intent.getStringExtra("join") != null -> Opening.Join(intent.getStringExtra("join").orEmpty(), intent.getStringExtra("direct-address"))
            else -> return state.emit(AppState.Waiting("No market yet: launch with a bundle and create or join"))
        }
        if (!bundle.isFile) return state.emit(AppState.Waiting("No coordination service bundle"))
        // Relays and discovery off: the voters and peers use the bundle's and contacts' addresses.
        val transport = if (loopback.isFile) PeerTransport.Iroh.isolated(bind = "0.0.0.0:0") else PeerTransport.Iroh()
        val secrets = KeystoreSecretStore(this)
        val random = SecureRandom()
        val platform = MarketPlatform(
            trustBundle = bundle.readText(),
            open = { how, commands ->
                val rocky = File(directory, "rocky")
                when (how) {
                    Opening.Create -> RockyDatabase.create(rocky, MarketSchema, transport, secrets, commands = commands)
                    is Opening.Join -> RockyDatabase.join(rocky, MarketSchema, how.invitation, transport, secrets, commands = commands)
                    Opening.Resume -> RockyDatabase.resume(rocky, MarketSchema, transport, secrets, commands = commands)
                }.also { lifecycle.follow(it) }
            },
            room = { Room.databaseBuilder<MarketDatabase>(this, File(directory, "market").absolutePath) },
            secrets = secrets,
            secureRandom = { ByteArray(it).also(random::nextBytes) },
        )
        state.emit(AppState.Opening)
        state.emit(
            try {
                val node = MarketNode.open(platform, opening)
                if (opening == Opening.Create) node.market.control()
                AppState.Open(node)
            } catch (error: Exception) {
                AppState.Waiting("Could not open: ${error.message}")
            },
        )
    }
}
