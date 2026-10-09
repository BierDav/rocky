package org.example.examples

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.lifecycle.lifecycleScope
import org.example.examples.shopping.ShoppingLinks
import org.example.examples.shopping.ui.ShoppingApp

/** The shared Compose app. The app's links pair this device or, in a debug build, drive it; see [ShoppingLinks]. */
class MainActivity : ComponentActivity() {
    private val app get() = application as ShoppingApplication
    private val links by lazy { ShoppingLinks(this, app.graph, app::chooseTransport, lifecycleScope) }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent { ShoppingApp(app.graph) }
        if (savedInstanceState == null) links.handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        links.handle(intent)
    }
}
