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
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingGraph

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
            Surface(Modifier.fillMaxSize()) {
                when (val state = graph.session.state.collectAsState().value) {
                    SessionState.Opening -> Opening()
                    SessionState.Setup, is SessionState.Failed -> SetupScreen()
                    is SessionState.Open -> MainScreen()
                    is SessionState.Revoked -> RevokedScreen()
                }
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

private val Leaf = Color(0xFF2E7D32)

@Composable
fun ShoppingTheme(content: @Composable () -> Unit) {
    val colors = if (isSystemInDarkTheme())
        darkColorScheme(primary = Color(0xFF8BD58F), secondary = Color(0xFFB8CCB4), tertiary = Color(0xFFA0CFD5))
    else lightColorScheme(primary = Leaf, secondary = Color(0xFF52634F), tertiary = Color(0xFF39656B),
        primaryContainer = Color(0xFFB7F1B5), secondaryContainer = Color(0xFFD5E8CF), tertiaryContainer = Color(0xFFBCEBF1))
    MaterialTheme(colorScheme = colors, content = content)
}
