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
import org.example.examples.shopping.listPartition

/**
 * The shared Compose screen. Driver extras, naming lists and items by title: `invitation` and `transport` (`tcp`
 * or iroh) on first open; then `contact` (logs it), `peer` (adds a contact), `online`, `add` (to `list`, else the
 * first list), `share` (logs one code per device, or for the node `to`), `accept` (a code), `check`, `move` (to the
 * list `to`), `sync`, `status` and `dump`; `step` logs `done <step>` or `failed <step>` last. See
 * docs/runtime/shopping-devices.md.
 */
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
        val extras = intent.extras ?: return
        app.scope.launch {
            try {
                val shopping = app.shopping().await()
                val repository = shopping.repository
                suspend fun list(title: String?) = repository.lists().first().let { lists ->
                    if (title == null) lists.firstOrNull()?.id else lists.firstOrNull { it.title == title }?.id
                }
                suspend fun item(title: String) = repository.lists().first().firstNotNullOfOrNull { list ->
                    repository.observe(list.id).first().firstOrNull { it.title == title }
                } ?: error("no item $title")
                if (extras.containsKey("contact")) shopping.runtime.contact().let { Log.i(TAG, "contact ${it.node} ${it.uri}") }
                extras.getString("peer")?.let { Log.i(TAG, "peer " + shopping.runtime.addPeer(it)) }
                extras.getString("online")?.let { shopping.runtime.setOnline(it.toBoolean()) }
                extras.getString("add")?.let { title ->
                    // Drivers add to the named or first list, which they create on first use.
                    val target = list(extras.getString("list")) ?: repository.createList(extras.getString("list") ?: "Shopping")
                    repository.add(target, title)
                }
                extras.getString("share")?.let { title ->
                    val target = list(title) ?: error("no list $title")
                    val codes = extras.getString("to")?.let { repository.shareCodes(target, listOf(it)) } ?: repository.shareWithDevices(target)
                    codes.forEach { (device, code) -> Log.i(TAG, "share $device $code") }
                }
                extras.getString("accept")?.let { repository.acceptCode(it) }
                extras.getString("check")?.let { title -> item(title).let { repository.check(it.list, it.id, !it.completed) } }
                extras.getString("move")?.let { title ->
                    val moved = item(title)
                    repository.move(moved.list, moved.id, list(extras.getString("to")) ?: error("no target list"))
                }
                if (extras.containsKey("sync")) Log.i(TAG, "sync " + shopping.runtime.syncOnce().state)
                if (extras.containsKey("status")) for (list in repository.lists().first()) {
                    Log.i(TAG, "status ${list.title}: " + shopping.runtime.partitions.status("list", listPartition(list.id)))
                }
                if (extras.containsKey("dump")) for (list in repository.lists().first()) {
                    val items = repository.observe(list.id).first()
                    Log.i(TAG, "dump ${list.title}: " + items.joinToString(",") { it.title + if (it.completed) " [x]" else "" })
                }
                extras.getString("step")?.let { Log.i(TAG, "done $it") }
            } catch (error: Exception) {
                extras.getString("step")?.let { Log.i(TAG, "failed $it ${error.message}") }
                failed(error)
            }
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
