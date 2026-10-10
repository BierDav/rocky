@file:OptIn(kotlinx.cinterop.ExperimentalForeignApi::class, kotlin.experimental.ExperimentalNativeApi::class)

package org.example.examples.shopping.ios

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.window.ComposeUIViewController
import androidx.room.Room
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import org.example.examples.shopping.*
import org.example.examples.shopping.ui.ShoppingApp
import org.example.examples.shopping.ui.ShoppingTheme
import platform.Foundation.NSApplicationSupportDirectory
import platform.Foundation.NSFileManager
import platform.Foundation.NSSearchPathForDirectoriesInDomains
import platform.Foundation.NSUUID
import platform.Foundation.NSUserDefaults
import platform.Foundation.NSUserDomainMask
import platform.UIKit.UIDevice
import rocky.CommandRuntime
import rocky.PeerTransport
import rocky.ServiceCoordinator
import rocky.ios.KeychainSecretStore
import rocky.ios.RockyLifecycle

// Reinstalling removes defaults, so a new installation cannot inherit an old owner's keys.
private val installationSecrets: KeychainSecretStore get() {
    val defaults = NSUserDefaults.standardUserDefaults
    val installation = defaults.stringForKey("keychain-installation") ?: NSUUID().UUIDString.also {
        defaults.setObject(it, "keychain-installation")
    }
    return KeychainSecretStore("org.example.rockyshopping.$installation")
}

private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
private val driverScope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
private var lifecycle: RockyLifecycle? = null

fun sceneVisibility(foreground: Boolean) {
    lifecycle?.sceneVisibility(foreground)
}

private var driverStatus by mutableStateOf<String?>(null)
private var pendingLink: String? = null
private var graph by mutableStateOf<ShoppingGraph?>(null)
private var resetting by mutableStateOf(false)
private var resetError by mutableStateOf<String?>(null)
private var driverGroup: String? = null
private val driverParts = mutableListOf<String>()
private val directory: String get() = (NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, true).first() as String) + "/shopping"

private fun installation(): ShoppingGraph = graph ?: run {
    val observer = RockyLifecycle.register().also { lifecycle = it }
    val visible = MutableStateFlow(observer.isForeground)
    ShoppingGraph(
        ShoppingPlatform(
            directory = directory,
            deviceName = UIDevice.currentDevice.name,
            room = { path -> Room.databaseBuilder<ShoppingDatabase>(name = path) },
            secrets = installationSecrets,
            transport = { if (kotlin.native.Platform.isDebugBinary && NSUserDefaults.standardUserDefaults.stringForKey("qualification-transport") == "tcp") PeerTransport.Tcp("127.0.0.1:0") else PeerTransport.Iroh(local = false) },
            commands = {
                val settings = NSUserDefaults.standardUserDefaults
                val bundle = settings.stringForKey("qualification-bundle")
                when {
                    kotlin.native.Platform.isDebugBinary && settings.stringForKey("qualification-transport") == "tcp" -> null
                    kotlin.native.Platform.isDebugBinary && bundle != null -> CommandRuntime(ServiceCoordinator(bundle), ApprovalHandlers)
                    else -> publicCoordination()
                }
            },
            opened = { database -> observer.follow(database, onApplied = { visible.value = it }, onError = { println("Shopping lifecycle: ${it.message}") }) },
            foreground = visible,
        ),
    ).also { graph = it }
}
fun viewController() = ComposeUIViewController {
    if (resetting) {
        ShoppingTheme {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                LinearProgressIndicator()
                Text("Resetting Shopping…")
            }
        }
        return@ComposeUIViewController
    }
    val installation = installation()
    LaunchedEffect(installation) {
        pendingLink?.let { url ->
            pendingLink = null
            offerLink(url)
        }
    }
    Column(Modifier.fillMaxSize().safeDrawingPadding()) {
        Box(Modifier.weight(1f)) { ShoppingApp(installation, reset = ::reset) }
        resetError?.let { ShoppingTheme { Text(it, Modifier.testTag("shopping-reset-error")) } }
        driverStatus?.let { Text(it, Modifier.testTag("qualification-result"), maxLines = 2) }
    }
}

private fun reset() {
    val previous = graph ?: return
    if (resetting || previous.session.state.value == SessionState.Opening) return
    resetting = true
    resetError = null
    scope.launch {
        try {
            driverScope.coroutineContext.job.children.toList().forEach { it.cancelAndJoin() }
            lifecycle?.unregister()
            lifecycle = null
            previous.session.close()
            withContext(Dispatchers.Default) {
                val files = NSFileManager.defaultManager
                if (files.fileExistsAtPath(directory)) check(files.removeItemAtPath(directory, null)) { "Could not remove local Shopping data" }
            }
            val defaults = NSUserDefaults.standardUserDefaults
            defaults.setObject(NSUUID().UUIDString, "keychain-installation")
            defaults.removeObjectForKey("qualification-transport")
            defaults.removeObjectForKey("qualification-bundle")
            pendingLink = null
            driverGroup = null
            driverParts.clear()
            driverStatus = null
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            resetError = "Could not reset: ${error.message}"
        } finally {
            graph = null
            resetting = false
        }
    }
}

/** External pairing links are offers; enrollment still needs the user's confirmation. */
fun offerLink(url: String) {
    if (resetting) return
    if (graph == null) {
        pendingLink = url
        return
    }
    val link = AppLink.parse(url) ?: return
    if (link.action == "drive-chunk" && kotlin.native.Platform.isDebugBinary) {
        val group = link.parameters["group"] ?: return
        val index = link.parameters["index"]?.toIntOrNull() ?: return
        val total = link.parameters["total"]?.toIntOrNull()?.takeIf { it in 1..256 } ?: return
        val part = link.parameters["part"]?.takeIf { it.length <= 400 } ?: return
        if (index == 0) {
            driverGroup = group
            driverParts.clear()
        }
        if (driverGroup != group || index != driverParts.size || index >= total) return
        driverParts += part
        if (driverParts.size == total) {
            val assembled = driverParts.joinToString("")
            driverParts.clear()
            driverGroup = null
            if (AppLink.parse(assembled)?.action == "drive") offerLink(assembled)
        } else {
            driverStatus = "staged $group $index"
        }
        return
    }
    if (link.action == PairingCode.ACTION) {
        runCatching { PairingCode.parse(url) }.onSuccess { graph?.session?.offer(it) }
    } else if (link.action == "drive" && kotlin.native.Platform.isDebugBinary) {
        val session = graph?.session ?: return
        driverScope.launch {
            val step = link.parameters["step"] ?: return@launch
            driverStatus = "started $step"
            try {
                if (link.parameters["configure"] == "true" && session.state.value == SessionState.Setup) {
                    NSUserDefaults.standardUserDefaults.setObject(link.parameters["transport"], "qualification-transport")
                    NSUserDefaults.standardUserDefaults.setObject(link.parameters["bundle"], "qualification-bundle")
                    driverStatus = "done $step"
                    return@launch
                }
                link.parameters["invitation"]?.let { invitation ->
                    if (session.state.value == SessionState.Setup) {
                        NSUserDefaults.standardUserDefaults.setObject(link.parameters["transport"], "qualification-transport")
                        session.join(if (invitation.startsWith("rocky://")) PairingCode(NodeKind.Full, "Simulator", invitation).toString() else invitation)
                    }
                }
                val opened = session.state.first { it is SessionState.Open || it is SessionState.Failed }
                val node = (opened as? SessionState.Open)?.node ?: throw (opened as SessionState.Failed).error
                val lines = mutableListOf<String>()
                driveShopping(node, link.parameters) {
                    lines += it
                    println("RockyShopping $it")
                }
                driverStatus = (lines + "done $step").joinToString("\n")
            } catch (error: Exception) {
                driverStatus = "failed $step ${error.message}"
            }
        }
    }
}
