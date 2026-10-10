@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, kotlinx.cinterop.BetaInteropApi::class, kotlin.experimental.ExperimentalNativeApi::class)

package org.example.examples.market.ios

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.window.ComposeUIViewController
import androidx.room.Room
import kotlinx.cinterop.addressOf
import kotlinx.cinterop.usePinned
import kotlinx.coroutines.*
import org.example.examples.market.MarketDatabase
import org.example.examples.market.MarketNode
import org.example.examples.market.MarketPlatform
import org.example.examples.market.MarketSchema
import org.example.examples.market.Opening
import org.example.examples.market.ui.MarketApp
import platform.Foundation.NSApplicationSupportDirectory
import platform.Foundation.NSFileManager
import platform.Foundation.NSSearchPathForDirectoriesInDomains
import platform.Foundation.NSString
import platform.Foundation.NSUTF8StringEncoding
import platform.Foundation.NSUUID
import platform.Foundation.NSUserDefaults
import platform.Foundation.NSUserDomainMask
import platform.Foundation.create
import platform.Foundation.stringWithContentsOfFile
import platform.Foundation.writeToFile
import platform.Security.SecRandomCopyBytes
import platform.Security.kSecRandomDefault
import rocky.*
import rocky.ios.KeychainSecretStore
import rocky.ios.RockyLifecycle

// Reinstalling removes defaults, so a new installation cannot inherit an old owner's keys.
private val installationSecrets: KeychainSecretStore get() {
    val defaults = NSUserDefaults.standardUserDefaults
    val installation = defaults.stringForKey("keychain-installation") ?: NSUUID().UUIDString.also {
        defaults.setObject(it, "keychain-installation")
    }
    return KeychainSecretStore("org.example.rockymarket.$installation")
}

private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
private var node by mutableStateOf<MarketNode?>(null)
private var message by mutableStateOf("Paste the service bundle, then create or join a market")
private var foreground by mutableStateOf(true)
private var opening by mutableStateOf(false)
private var lifecycle: RockyLifecycle? = null
fun sceneVisibility(foreground: Boolean) {
    lifecycle?.sceneVisibility(foreground)
}

private val directory: String get() = (NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, true).first() as String) + "/market"

fun viewController() = ComposeUIViewController {
    if (lifecycle == null) {
        lifecycle = RockyLifecycle.register()
        if (RockyDatabase.exists("$directory/rocky")) {
            val saved = NSString.stringWithContentsOfFile("$directory/bundle.json", NSUTF8StringEncoding, null)
            if (saved != null) open(saved, Opening.Resume) else message = "Missing coordination bundle for this installation"
        }
    }
    MaterialTheme { MarketApp(node, message, foreground, busy = opening, reset = ::reset, open = ::open) }
}

private fun reset() {
    if (opening) return
    opening = true
    message = "Resetting market"
    val previous = node
    node = null
    scope.launch {
        try {
            previous?.let {
                lifecycle?.unfollow(it.rocky)
                it.close()
            }
            withContext(Dispatchers.Default) {
                val files = NSFileManager.defaultManager
                if (files.fileExistsAtPath(directory)) check(files.removeItemAtPath(directory, null)) { "Could not remove local market data" }
            }
            NSUserDefaults.standardUserDefaults.setObject(NSUUID().UUIDString, "keychain-installation")
            message = "Paste the service bundle, then create or join a market"
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            message = "Could not reset: ${error.message}"
        } finally {
            opening = false
        }
    }
}

private fun open(bundle: String, how: Opening) {
    if (opening || node != null) return
    opening = true
    message = "Opening market"
    scope.launch {
        var pending: MarketNode? = null
        try {
            val secrets = installationSecrets
            val transport = if (kotlin.native.Platform.isDebugBinary && NSUserDefaults.standardUserDefaults.boolForKey("qualification-loopback")) {
                PeerTransport.Iroh.isolated()
            } else {
                PeerTransport.Iroh(local = false)
            }
            val platform = MarketPlatform(
                trustBundle = bundle,
                secrets = secrets,
                secureRandom = { size ->
                    ByteArray(size).also { bytes ->
                        if (size > 0) bytes.usePinned { check(SecRandomCopyBytes(kSecRandomDefault, size.toULong(), it.addressOf(0)) == 0) { "Secure random source failed" } }
                    }
                },
                room = { Room.databaseBuilder<MarketDatabase>(name = "$directory/market") },
                open = { action, commands ->
                    withContext(Dispatchers.Default) {
                        when (action) {
                            Opening.Create -> RockyDatabase.create("$directory/rocky", MarketSchema, transport, secrets, commands = commands)
                            is Opening.Join -> RockyDatabase.join("$directory/rocky", MarketSchema, action.invitation, transport, secrets, commands = commands)
                            Opening.Resume -> RockyDatabase.resume("$directory/rocky", MarketSchema, transport, secrets, commands = commands)
                        }
                    }.also { database ->
                        lifecycle?.follow(
                            database,
                            onApplied = { visible -> scope.launch { foreground = visible } },
                            onError = { error -> scope.launch { message = "Lifecycle: ${error.message}" } },
                        )
                    }
                },
            )
            NSFileManager.defaultManager.createDirectoryAtPath(directory, true, null, null)
            check(NSString.create(string = bundle).writeToFile("$directory/bundle.json", true, NSUTF8StringEncoding, null)) { "Could not save coordination bundle" }
            val opened = MarketNode.open(platform, how).also { pending = it }
            if (how == Opening.Create) opened.market.control()
            node = opened
            pending = null
            message = ""
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            message = "Could not open: ${error.message}"
        } finally {
            pending?.close()
            opening = false
        }
    }
}
