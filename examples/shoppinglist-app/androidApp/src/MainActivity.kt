package org.example.examples

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.ShoppingRepository
import org.example.examples.shopping.ShoppingScreen

/** The shared Compose screen. Extras for drivers: `invitation`, `transport` (`tcp` or iroh) and `add`, to the first list. */
class MainActivity : ComponentActivity() {
    private val app get() = application as ShoppingApplication
    private var log: Job? = null
    private val localNetwork = registerForActivityResult(ActivityResultContracts.RequestPermission()) { open() }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        // A denial still opens: iroh reaches peers through relays, only LAN routes are refused.
        if (Build.VERSION.SDK_INT >= 37 && checkSelfPermission(LOCAL_NETWORK) != PackageManager.PERMISSION_GRANTED) {
            localNetwork.launch(LOCAL_NETWORK)
        } else open()
    }

    private fun open() {
        val shopping = app.shopping(intent.getStringExtra("invitation"), intent.getStringExtra("transport"))
        setContent {
            val opened by produceState<Result<ShoppingRepository>?>(null) {
                value = runCatching { shopping.await().repository }
            }
            when (val result = opened) {
                null -> Text("Opening…")
                else -> result.fold({ ShoppingScreen(it, ::failed) }, { Text("Cannot open: ${it.message}") })
            }
        }
        log = app.scope.launch {
            val repository = try { shopping.await().repository } catch (error: Exception) {
                Log.e(TAG, "open failed", error)
                return@launch
            }
            repository.lists().collect { rows ->
                Log.i(TAG, "lists " + rows.joinToString(",", "[", "]") { it.id.toString() + ":" + it.title })
            }
        }
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    override fun onStart() { super.onStart(); Log.i(TAG, "foreground") }
    override fun onStop() { Log.i(TAG, "background"); super.onStop() }
    override fun onDestroy() { log?.cancel(); super.onDestroy() }

    private fun handle(intent: Intent) {
        val title = intent.getStringExtra("add") ?: return
        app.scope.launch {
            try {
                val repository = app.shopping().await().repository
                // Drivers add to the first list, which they create on first use.
                val list = repository.lists().first().firstOrNull()?.id ?: repository.createList("Shopping")
                repository.add(list, title)
            } catch (error: Exception) { failed(error) }
        }
    }

    private fun failed(error: Throwable) {
        Log.e(TAG, "write failed", error)
        Toast.makeText(this, "Failed: ${error.message}", Toast.LENGTH_LONG).show()
    }

    companion object {
        const val TAG = "RockyShopping"
        private const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
    }
}
