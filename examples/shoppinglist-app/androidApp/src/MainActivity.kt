package org.example.examples

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import org.example.examples.shopping.presentation.host.ShoppingApp

/** The shared Compose app; links the system opens here go to its deep links. */
class MainActivity : ComponentActivity() {
    private val app get() = application as ShoppingApplication

    override fun onCreate(savedInstanceState: Bundle?) {
        // The system splash stays until the shared app has drawn its first settled screen.
        installSplashScreen().setKeepOnScreenCondition { app.appGraph.splash.visible.value }
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            ShoppingApp(app.appGraph)
        }
        // A recreated activity already restored its back stack, including an opened link.
        if (savedInstanceState == null) openLink(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openLink(intent)
    }

    private fun openLink(intent: Intent?) {
        intent?.dataString?.let(app.appGraph.deepLinks::open)
    }
}
