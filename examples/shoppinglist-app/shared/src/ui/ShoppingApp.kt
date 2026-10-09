package org.example.examples.shopping.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner
import dev.zacsweers.metrox.viewmodel.LocalMetroViewModelFactory
import org.example.examples.shopping.PairingCode
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingGraph
import org.example.examples.shopping.ShoppingSession

/** The whole app: setup until this device belongs to a household, then its lists, devices and sync. */
@Composable
fun ShoppingApp(graph: ShoppingGraph) {
    // A desktop window may not own ViewModels; the app then keeps them for its lifetime.
    val parentOwner = LocalViewModelStoreOwner.current
    val owner = parentOwner ?: remember { object : ViewModelStoreOwner { override val viewModelStore = ViewModelStore() } }
    DisposableEffect(owner) {
        onDispose { if (parentOwner == null) owner.viewModelStore.clear() }
    }
    CompositionLocalProvider(LocalMetroViewModelFactory provides graph.metroViewModelFactory, LocalViewModelStoreOwner provides owner) {
        ShoppingTheme {
            NetworkPermissions()
            Surface(Modifier.fillMaxSize()) {
                val state = graph.session.state.collectAsState().value
                when (state) {
                    SessionState.Opening -> Opening()
                    SessionState.Setup, is SessionState.Failed -> SetupScreen()
                    is SessionState.Open -> MainScreen()
                    is SessionState.Revoked -> RevokedScreen()
                }
                graph.session.offer.collectAsState().value?.let { offer -> OfferDialog(offer, state, graph.session) }
            }
        }
    }
}

@Composable
private fun Opening() {
    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator()
        Spacer(Modifier.height(16.dp))
        Text("Opening the household…", style = MaterialTheme.typography.bodyLarge)
        Text("Enrolling, installing authors and starting sync", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** A pairing link opened from outside, such as a camera app: the user confirms it before this device joins. */
@Composable
private fun OfferDialog(offer: PairingCode, state: SessionState, session: ShoppingSession) {
    val joinable = state == SessionState.Setup || state is SessionState.Revoked
    AlertDialog(onDismissRequest = session::dismissOffer,
        title = { Text(if (joinable) "Join as ${offer.name.ifBlank { "this device" }}?" else "Already in a household") },
        text = {
            Text(if (joinable) "A trusted device invited this one as a ${offer.kind.label.lowercase()} device: ${offer.kind.summary.lowercase()}." +
                (if (state is SessionState.Revoked) " The revoked copy is kept aside and this device gets a new identity." else "")
            else "This device already belongs to a household. A pairing code only sets up a new or revoked device.")
        },
        confirmButton = { if (joinable) Button(session::acceptOffer) { Text("Join") } else TextButton(session::dismissOffer) { Text("OK") } },
        dismissButton = { if (joinable) TextButton(session::dismissOffer) { Text("Cancel") } })
}

private val Leaf = Color(0xFF2E7D32)

@Composable
fun ShoppingTheme(content: @Composable () -> Unit) {
    val colors = if (isSystemInDarkTheme())
        darkColorScheme(primary = Color(0xFF8BD58F), secondary = Color(0xFFB8CCB4), tertiary = Color(0xFFA0CFD5))
    else lightColorScheme(primary = Leaf, secondary = Color(0xFF52634F), tertiary = Color(0xFF39656B),
        primaryContainer = Color(0xFFB7F1B5), secondaryContainer = Color(0xFFD5E8CF), tertiaryContainer = Color(0xFFBCEBF1))
    MaterialTheme(colorScheme = colors, content = content)
}
