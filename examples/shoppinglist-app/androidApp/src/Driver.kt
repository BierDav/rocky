package org.example.examples

import android.content.Intent
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import org.example.examples.ShoppingApplication.Companion.TAG
import org.example.examples.shopping.*

/**
 * Intent extras that drive the app for device qualification, naming lists and items by title: `invitation` (a
 * pairing code or a raw invitation) and `transport` (`tcp` or iroh) on first open; then `contact` (logs it), `peer`
 * (adds a contact), `online`, `add` (to `list`, else the first list), `share` (logs one code per device, or for the
 * node `to`), `accept` (a code), `check`, `move` (to the list `to`), `sync`, `status` and `dump`. `step` logs
 * `done <step>` or `failed <step>` last. See docs/runtime/shopping-devices.md.
 */
internal class Driver(private val app: ShoppingApplication, private val scope: CoroutineScope) {
    fun handle(intent: Intent) {
        val extras = intent.extras ?: return
        val session = app.graph.session
        scope.launch {
            try {
                extras.getString("invitation")?.let { invitation ->
                    if (session.state.value == SessionState.Setup) {
                        app.chooseTransport(extras.getString("transport"))
                        // A raw invitation joins with the grants it carries, as a full replica.
                        session.join(if (invitation.startsWith("rocky://")) PairingCode(NodeKind.Full, "Phone", invitation).toString() else invitation)
                    }
                }
                val opened = session.state.first { it is SessionState.Open || it is SessionState.Failed }
                val node = (opened as? SessionState.Open)?.node ?: throw (opened as SessionState.Failed).error
                run(node, extras)
                extras.getString("step")?.let { Log.i(TAG, "done $it") }
            } catch (error: Exception) {
                extras.getString("step")?.let { Log.i(TAG, "failed $it ${error.message}") }
                Log.e(TAG, "driver step failed", error)
            }
        }
    }

    private suspend fun run(node: ShoppingNode, extras: android.os.Bundle) {
        val repository = node.repository
        val rocky = node.rocky
        suspend fun list(title: String?) = repository.lists().first().let { lists ->
            if (title == null) lists.firstOrNull()?.id else lists.firstOrNull { it.title == title }?.id
        }
        suspend fun item(title: String) = repository.lists().first().firstNotNullOfOrNull { list ->
            repository.observe(list.id).first().firstOrNull { it.title == title }
        } ?: error("no item $title")
        if (extras.containsKey("contact")) rocky.contact().let { Log.i(TAG, "contact ${it.node} ${it.uri}") }
        extras.getString("peer")?.let { Log.i(TAG, "peer " + rocky.addPeer(it)) }
        extras.getString("online")?.let { rocky.setOnline(it.toBoolean()) }
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
        if (extras.containsKey("sync")) Log.i(TAG, "sync " + rocky.syncOnce().state)
        if (extras.containsKey("status")) for (list in repository.lists().first()) {
            Log.i(TAG, "status ${list.title}: " + rocky.list(list.id).status())
        }
        if (extras.containsKey("dump")) for (list in repository.lists().first()) {
            val items = repository.observe(list.id).first()
            Log.i(TAG, "dump ${list.title}: " + items.joinToString(",") { it.title + if (it.completed) " [x]" else "" })
        }
    }
}
