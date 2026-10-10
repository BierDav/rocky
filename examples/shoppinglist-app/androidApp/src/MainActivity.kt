package org.example.examples

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import org.example.examples.shopping.SessionState
import org.example.examples.shopping.ShoppingGraph
import org.example.examples.shopping.ShoppingLinks
import org.example.examples.shopping.ui.ShoppingApp

/** The shared Compose app. The app's links pair this device or, in a debug build, drive it; see [ShoppingLinks]. */
class MainActivity : ComponentActivity() {
    private val app get() = application as ShoppingApplication
    private var linksGraph: ShoppingGraph? = null
    private var linksHandler: ShoppingLinks? = null
    private val links: ShoppingLinks get() {
        if (linksGraph !== app.graph) {
            linksGraph = app.graph
            linksHandler = ShoppingLinks(this, app.graph, app::chooseTransport, lifecycleScope)
        }
        return checkNotNull(linksHandler)
    }
    private var resetting by mutableStateOf(false)
    private var resetError by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            Column(Modifier.fillMaxSize().safeDrawingPadding().semantics { testTagsAsResourceId = true }) {
                if (resetting) {
                    LinearProgressIndicator()
                    Text("Resetting Shopping…")
                } else {
                    Box(Modifier.weight(1f)) { ShoppingApp(app.graph, reset = ::reset) }
                }
                resetError?.let { Text(it, Modifier.testTag("shopping-reset-error")) }
                links.status.collectAsState().value?.let { Text(it, Modifier.testTag("qualification-result"), maxLines = 2) }
            }
        }
        if (savedInstanceState == null) links.handle(intent)
    }

    private fun reset() {
        if (resetting || app.graph.session.state.value == SessionState.Opening) return
        resetting = true
        resetError = null
        lifecycleScope.launch {
            try {
                links.close()
                app.reset()
                recreate()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                resetError = "Could not reset: ${error.message}"
                resetting = false
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (!resetting) links.handle(intent)
    }
}
