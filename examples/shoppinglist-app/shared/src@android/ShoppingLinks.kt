package org.example.examples.shopping

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.util.Log
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * Opens the app's links from an activity's intents. `rocky-shopping://pair?…` is a [PairingCode], offered for
 * confirmation. `rocky-shopping://drive?…` drives a debuggable build for device qualification, naming lists and items
 * by title: `invitation` (a pairing code or a raw invitation) and `transport` (`tcp` or iroh) on first open; then
 * `contact` (logs it), `peer` (adds a contact), `forget` (removes a peer), `online`, `add` (to `list`, else the first list), `share` (logs one
 * code per device, or for the node `to`), `accept` (a code), `check`, `move` (to the list `to`), `sync`, `status` and
 * `dump`. `step` delimits the log with `started <step>` and `done <step>` or `failed <step>`. See docs/runtime/shopping-devices.md.
 */
class ShoppingLinks(
    private val context: Context,
    private val graph: ShoppingGraph,
    /** Records the transport the next create or join uses. */
    private val chooseTransport: (String?) -> Unit,
    scope: CoroutineScope,
) {
    private val work = CoroutineScope(scope.coroutineContext + SupervisorJob(scope.coroutineContext[Job]))
    private val result = MutableStateFlow<String?>(null)
    val status = result.asStateFlow()
    private val debuggable get() = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0

    /** Stops this handler's drivers before replacing its installation. */
    suspend fun close() = work.coroutineContext.job.cancelAndJoin()

    fun handle(intent: Intent?) {
        val link = intent?.dataString?.let(AppLink::parse) ?: return
        when (link.action) {
            PairingCode.ACTION -> runCatching { PairingCode.parse(link.toString()) }
                .onSuccess(graph.session::offer)
                .onFailure { Log.w(TAG, "ignored pairing link: ${it.message}") }

            DRIVE -> if (debuggable) drive(link.parameters) else Log.w(TAG, "drive links need a debuggable build")
        }
    }

    private fun drive(parameters: Map<String, String>) {
        val session = graph.session
        work.launch {
            try {
                parameters["step"]?.let {
                    Log.i(TAG, "started $it")
                    result.value = "started $it"
                }
                val lines = mutableListOf<String>()
                parameters["invitation"]?.let { invitation ->
                    if (session.state.value == SessionState.Setup) {
                        chooseTransport(parameters["transport"])
                        // A raw invitation joins with the grants it carries, as a full replica.
                        session.join(if (invitation.startsWith("rocky://")) PairingCode(NodeKind.Full, "Phone", invitation).toString() else invitation)
                    }
                }
                val opened = session.state.first { it is SessionState.Open || it is SessionState.Failed }
                val node = (opened as? SessionState.Open)?.node ?: throw (opened as SessionState.Failed).error
                driveShopping(node, parameters) {
                    lines += it
                    Log.i(TAG, it)
                }
                parameters["step"]?.let {
                    Log.i(TAG, "done $it")
                    result.value = (lines + "done $it").joinToString("\n")
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                parameters["step"]?.let {
                    Log.i(TAG, "failed $it ${error.message}")
                    result.value = "failed $it ${error.message}"
                }
                Log.e(TAG, "driver step failed", error)
            }
        }
    }

    companion object {
        const val TAG = "RockyShopping"
        const val DRIVE = "drive"
    }
}
