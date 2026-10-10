package org.example.examples.shopping

import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import rocky.RockyEvent

/** Debug qualification actions, called only after the platform checks its build is debuggable. */
suspend fun driveShopping(node: ShoppingNode, parameters: Map<String, String>, log: (String) -> Unit) {
    val repository = node.repository
    val rocky = node.rocky
    suspend fun list(title: String?) = repository.lists().first().let { lists ->
        if (title == null) lists.firstOrNull()?.id else lists.firstOrNull { it.title == title }?.id
    }
    suspend fun item(title: String) = repository.lists().first().firstNotNullOfOrNull { list ->
        repository.observe(list.id).first().firstOrNull { it.title == title }
    } ?: error("no item $title")
    if ("contact" in parameters) rocky.contact().let { log("contact ${it.node} ${it.uri}") }
    parameters["peer"]?.let { log("peer " + rocky.addPeer(it)) }
    parameters["forget"]?.let { rocky.removePeer(it) }
    if (parameters["background"] == "true") rocky.enterBackground()
    parameters["online"]?.let { rocky.setOnline(it.toBoolean()) }
    parameters["add"]?.let { title ->
        // Drivers add to the named or first list, which they create on first use.
        val target = list(parameters["list"]) ?: repository.createList(parameters["list"] ?: "Shopping")
        repository.add(target, title)
    }
    parameters["share"]?.let { title ->
        val target = list(title) ?: error("no list $title")
        val devices = parameters["to"]?.let { listOf(it).also { to -> repository.share(target, to) } } ?: repository.shareWithDevices(target)
        devices.forEach { device -> log("share $device") }
    }
    parameters["check"]?.let { title -> item(title).let { repository.check(it.list, it.id, !it.completed) } }
    parameters["move"]?.let { title ->
        val moved = item(title)
        repository.move(moved.list, moved.id, list(parameters["to"]) ?: error("no target list"))
    }
    if ("sync" in parameters) {
        rocky.syncOnce().let { status ->
            log("sync ${status.state} ${status.error.orEmpty()}")
            rocky.peers().forEach { peer -> log("sync peer ${peer.node} ${peer.state} ${peer.error.orEmpty()}") }
        }
    }
    if ("status" in parameters) {
        for (list in repository.lists().first()) {
            log("status ${list.title}: " + rocky.list(list.id).status())
        }
    }
    if ("events" in parameters) {
        withTimeoutOrNull(1_000) {
            rocky.activity().filterIsInstance<RockyEvent.EnvelopeRefused>().collect {
                log("envelope refused ${it.error}")
            }
        }
    }
    if ("dump" in parameters) {
        for (list in repository.lists().first()) {
            val items = repository.observe(list.id).first()
            log("dump ${list.title}: " + items.joinToString(",") { it.title + if (it.completed) " [x]" else "" })
        }
    }
}
