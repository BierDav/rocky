package org.example.examples.market

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import org.example.examples.market.ui.MarketApp

/** The shared screen once the market is open. */
class MainActivity : ComponentActivity() {
    private var foreground by mutableStateOf(false)
    private val app get() = application as MarketApplication

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize().safeDrawingPadding().semantics { testTagsAsResourceId = true }) {
                    val state = app.current.collectAsState().value
                    MarketApp((state as? AppState.Open)?.node, (state as? AppState.Waiting)?.message ?: if (state is AppState.Opening) "Opening market" else "", foreground, busy = state is AppState.Opening) { bundle, how ->
                        lifecycleScope.launch { app.openFromUi(bundle, how) }
                    }
                }
            }
        }
        lifecycleScope.launch { app.open(intent) }
    }

    override fun onStart() {
        super.onStart()
        foreground = true
    }

    override fun onStop() {
        foreground = false
        super.onStop()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        lifecycleScope.launch { app.open(intent) }
    }
}
