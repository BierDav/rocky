package org.example.examples.market

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import org.example.examples.market.ui.MarketScreen

/** The shared screen once the market is open. */
class MainActivity : ComponentActivity() {
    private val app get() = application as MarketApplication

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize().safeDrawingPadding()) {
                    when (val state = app.current.collectAsState().value) {
                        is AppState.Open -> MarketScreen(state.node.market)
                        is AppState.Waiting -> Text(state.message, Modifier.padding(16.dp))
                    }
                }
            }
        }
        lifecycleScope.launch { app.open(intent) }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        lifecycleScope.launch { app.open(intent) }
    }
}
