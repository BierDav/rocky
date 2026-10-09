package org.example.examples.shopping

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * Opens the app's links from an activity's intents. `rocky-shopping://pair?…` is a [PairingCode], offered for
 * confirmation. `rocky-shopping://drive?…` drives a debuggable build for device qualification, naming lists and items
 * by title: `invitation` (a pairing code or a raw invitation) and `transport` (`tcp` or iroh) on first open; then
 * `contact` (logs it), `peer` (adds a contact), `online`, `add` (to `list`, else the first list), `share` (logs one
 * code per device, or for the node `to`), `accept` (a code), `check`, `move` (to the list `to`), `sync`, `status` and
 * `dump`. `step` logs `done <step>` or `failed <step>` last. See docs/runtime/shopping-devices.md.
 */
class ShoppingLinks(
    private val context: Context,
    private val graph: ShoppingGraph,
    /** Records the transport the next create or join uses. */
    private val chooseTransport: (String?) -> Unit,
    private val scope: CoroutineScope,
) {
    private val debuggable get() = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0

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
        scope.launch {
            try {
                parameters["invitation"]?.let { invitation ->
                    if (session.state.value == SessionState.Setup) {
                        chooseTransport(parameters["transport"])
                        // A raw invitation joins with the grants it carries, as a full replica.
                        session.join(if (invitation.startsWith("rocky://")) PairingCode(NodeKind.Full, "Phone", invitation).toString() else invitation)
                    }
                }
                val opened = session.state.first { it is SessionState.Open || it is SessionState.Failed }
                val node = (opened as? SessionState.Open)?.node ?: throw (opened as SessionState.Failed).error
                run(node, parameters)
                parameters["step"]?.let { Log.i(TAG, "done $it") }
            } catch (error: Exception) {
                parameters["step"]?.let { Log.i(TAG, "failed $it ${error.message}") }
                Log.e(TAG, "driver step failed", error)
            }
        }
    }

    private suspend fun run(node: ShoppingNode, parameters: Map<String, String>) {
        val repository = node.repository
        val rocky = node.rocky
        suspend fun list(title: String?) = repository.lists().first().let { lists ->
            if (title == null) lists.firstOrNull()?.id else lists.firstOrNull { it.title == title }?.id
        }
        suspend fun item(title: String) = repository.lists().first().firstNotNullOfOrNull { list ->
            repository.observe(list.id).first().firstOrNull { it.title == title }
        } ?: error("no item $title")
        if ("contact" in parameters) rocky.contact().let { Log.i(TAG, "contact ${it.node} ${it.uri}") }
        parameters["peer"]?.let { Log.i(TAG, "peer " + rocky.addPeer(it)) }
        parameters["online"]?.let { rocky.setOnline(it.toBoolean()) }
        parameters["add"]?.let { title ->
            // Drivers add to the named or first list, which they create on first use.
            val target = list(parameters["list"]) ?: repository.createList(parameters["list"] ?: "Shopping")
            repository.add(target, title)
        }
        parameters["share"]?.let { title ->
            val target = list(title) ?: error("no list $title")
            val codes = parameters["to"]?.let { repository.shareCodes(target, listOf(it)) } ?: repository.shareWithDevices(target)
            codes.forEach { (device, code) -> Log.i(TAG, "share $device $code") }
        }
        parameters["accept"]?.let { repository.acceptCode(it) }
        parameters["check"]?.let { title -> item(title).let { repository.check(it.list, it.id, !it.completed) } }
        parameters["move"]?.let { title ->
            val moved = item(title)
            repository.move(moved.list, moved.id, list(parameters["to"]) ?: error("no target list"))
        }
        if ("sync" in parameters) Log.i(TAG, "sync " + rocky.syncOnce().state)
        if ("status" in parameters) for (list in repository.lists().first()) {
            Log.i(TAG, "status ${list.title}: " + rocky.list(list.id).status())
        }
        if ("dump" in parameters) for (list in repository.lists().first()) {
            val items = repository.observe(list.id).first()
            Log.i(TAG, "dump ${list.title}: " + items.joinToString(",") { it.title + if (it.completed) " [x]" else "" })
        }
    }

    companion object {
        const val TAG = "RockyShopping"
        const val DRIVE = "drive"
    }
}
