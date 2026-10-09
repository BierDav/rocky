package org.example.examples

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.lifecycleScope
import org.example.examples.shopping.ui.ShoppingApp

/** The shared Compose app. Intent extras drive it for device qualification; see [Driver]. */
class MainActivity : ComponentActivity() {
    private val app get() = application as ShoppingApplication
    private val driver by lazy { Driver(app, lifecycleScope) }
    // A denial still works: iroh reaches peers through relays, only direct LAN routes are refused.
    private val localNetwork = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 37 && checkSelfPermission(LOCAL_NETWORK) != PackageManager.PERMISSION_GRANTED) {
            localNetwork.launch(LOCAL_NETWORK)
        }
        setContent { ShoppingApp(app.graph) }
        driver.handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        driver.handle(intent)
    }

    private companion object {
        const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
    }
}
