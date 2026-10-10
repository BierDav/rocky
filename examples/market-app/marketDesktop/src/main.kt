package org.example.examples.market.desktop

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.rememberWindowState
import androidx.room.Room
import kotlinx.coroutines.runBlocking
import org.example.examples.market.Market
import org.example.examples.market.MarketDatabase
import org.example.examples.market.MarketNode
import org.example.examples.market.MarketPlatform
import org.example.examples.market.MarketSchema
import org.example.examples.market.Opening
import org.example.examples.market.ui.MarketScreen
import rocky.PeerTransport
import rocky.RockyDatabase
import rocky.SecretStore
import java.io.File
import java.nio.file.Files
import java.nio.file.attribute.PosixFilePermissions
import java.security.SecureRandom

/** Owner-only files in [directory]; the market's tenant key and pending controls. */
class FileSecrets(private val directory: File) : SecretStore {
    override fun load(name: String): ByteArray? = File(directory, name).takeIf { it.isFile }?.readBytes()
    override fun store(name: String, secret: ByteArray) {
        directory.mkdirs()
        val file = File(directory, "$name.tmp")
        Files.write(file.toPath(), secret)
        runCatching { Files.setPosixFilePermissions(file.toPath(), PosixFilePermissions.fromString("rw-------")) }
        Files.move(file.toPath(), File(directory, name).toPath(), java.nio.file.StandardCopyOption.ATOMIC_MOVE)
    }
    override fun delete(name: String) {
        File(directory, name).delete()
    }
}

private const val USAGE = "Usage: Market [--create | --join INVITATION] [--invite FILE]"

/**
 * The market lives in `ROCKY_MARKET_DATA`, else `~/.rocky-market`; `ROCKY_MARKET_BUNDLE` names the coordination
 * service's trust bundle. `--invite FILE` writes a buyer invitation once the market is open.
 * `ROCKY_MARKET_LOOPBACK=1` binds iroh on loopback without relays, for `scripts/coordinator-local`.
 */
fun main(args: Array<String>) {
    val options = args.toList().chunked(2).associate { it[0] to it.getOrNull(1) }
    require(options.keys.all { it in setOf("--create", "--join", "--invite") } && ("--join" !in options || options["--join"] != null)) { USAGE }
    val directory = File(System.getenv("ROCKY_MARKET_DATA") ?: File(System.getProperty("user.home"), ".rocky-market").path)
    val bundle = File(System.getenv("ROCKY_MARKET_BUNDLE") ?: error("set ROCKY_MARKET_BUNDLE to the service's bundle.json")).readText()
    val transport = if (System.getenv("ROCKY_MARKET_LOOPBACK") == "1") PeerTransport.Iroh.isolated(bind = "127.0.0.1:0") else PeerTransport.Iroh()
    val random = SecureRandom()
    val platform = MarketPlatform(
        trustBundle = bundle,
        open = { opening, commands ->
            val rocky = File(directory, "rocky")
            when (opening) {
                Opening.Create -> RockyDatabase.create(rocky, MarketSchema, transport, commands = commands)
                is Opening.Join -> RockyDatabase.join(rocky, MarketSchema, opening.invitation, transport, commands = commands)
                Opening.Resume -> RockyDatabase.resume(rocky, MarketSchema, transport, commands = commands)
            }
        },
        room = { Room.databaseBuilder<MarketDatabase>(File(directory, "market").absolutePath) },
        secrets = FileSecrets(File(directory, "app-secrets")),
        secureRandom = { ByteArray(it).also(random::nextBytes) },
    )
    val opening = when {
        "--create" in options -> Opening.Create
        "--join" in options -> Opening.Join(checkNotNull(options["--join"]))
        else -> Opening.Resume
    }
    val node = runBlocking {
        MarketNode.open(platform, opening).also { node ->
            if (opening == Opening.Create) node.market.control()
            options["--invite"]?.let { File(it).writeText(node.rocky.pairing(Market.Buyer).uri) }
        }
    }
    application {
        Window(
            onCloseRequest = {
                runBlocking { node.close() }
                exitApplication()
            },
            title = "Market",
            state = rememberWindowState(width = 480.dp, height = 720.dp),
        ) { MaterialTheme { MarketScreen(node.market) } }
    }
}
