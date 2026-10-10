package consumer

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import rocky.*
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicBoolean

private val amount = Shape(listOf(Field("amount", FieldType.Integer)), { n: Long -> listOf(n) }, { it.single() as Long })
private val add = Command(
    "Add",
    handler = "items.add.v1",
    arguments = amount,
    accepted = amount,
    rejected = Shape.none,
    invocation = "items",
    result = "items",
    resources = listOf("items"),
)
private val schema = schema("packaged") {
    table("items") {
        text("title")
        primaryKey("id")
    }
    commands = CommandSet(listOf(add))
}
private val table = schema.table("items")
private fun handle(n: Int) = n.toString(16).padStart(64, '0')
private suspend fun titles(port: RuntimePort) = Row.decode(port.read(table.query())).map { it.text("title") }

/** Credential primitives and a non-issuer executor, using only the packaged library. */
private suspend fun credentialPrimitives(root: File) {
    val coordinator = DevelopmentCoordinator()
    val issuer = RockyDatabase.local(File(root, "issuer"), schema, commands = CommandRuntime(coordinator))
    var executor: RockyDatabase? = null
    var delegate: RockyDatabase? = null
    try {
        val keys = Credentials.prepare(File(root, "executor"))
        val all = listOf(Grant("items", setOf(Right.Read, Right.Insert, Right.Update, Right.Delete, Right.Invoke, Right.Execute, Right.Custody)))
        val x = RockyDatabase.install(
            File(root, "executor"),
            schema,
            issuer.credentials.grant(keys, all, delegations = 2, authors = 2),
            commands = CommandRuntime(coordinator, listOf(add handledBy Handler { _, n -> Decision.accept(n) })),
        ).also { executor = it }
        check(!x.isIssuer && x.commandIdentity().executes)
        coordinator.register(x.commandIdentity())
        x.commands.submit(add, 11L, handle(20))
        check(x.commands.run(add, handle(20)).outcome == Outcome.Accepted(11L))
        val asked = x.credentials.pseudonymRequest("items")
        val credential = issuer.credentials.assignPseudonym(asked)
        val author = x.credentials.installPseudonym(credential)
        // The registry uses author 1; the delegate's range reserves 2 and 3.
        check(author == AuthorPseudonym("items", 4))
        check(x.credentials.installPseudonym(issuer.credentials.assignPseudonym(asked)) == author)
        check(issuer.credentials.pseudonymOwner(credential).node == x.session().node)
        check(runCatching { x.credentials.pseudonymOwner(credential) }.exceptionOrNull() is RockyException)
        val delegated = Credentials.prepare(File(root, "delegate"))
        val writes = listOf(Grant("items", setOf(Right.Read, Right.Insert, Right.Custody)))
        val d = RockyDatabase.install(
            File(root, "delegate"),
            schema,
            x.credentials.delegate(delegated, writes, authors = 1),
        ).also { delegate = it }
        check(!d.isIssuer)
        // Delegating one author reserves 2; x assigns the next free number, 3.
        check(d.credentials.installPseudonym(x.credentials.assignPseudonym(d.credentials.pseudonymRequest("items"))) == AuthorPseudonym("items", 3))
    } finally {
        delegate?.close()
        executor?.close()
        issuer.close()
    }
}

/** The shopping schema's group, title policy and references, declared and installed from the packaged library. */
private suspend fun groupedLists(root: File) {
    val shopping = schema("shopping", version = 3) {
        table("lists") {
            text("title")
            primaryKey("id")
            partitionBy("id", group = "list")
        }
        table("categories") {
            integer("list")
            text("name")
            primaryKey("list", "id")
            partitionBy("list", group = "list")
            reference("list", "lists", onDelete = Delete.Cascade)
        }
        table("items") {
            integer("list")
            text("key")
            text("title")
            integer("category", nullable = true)
            primaryKey("list", "key")
            partitionBy("list", group = "list")
            unique("list", "title", onConflict = Conflict.LatestWins)
            reference("list", "lists", onDelete = Delete.Cascade)
            reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
        }
    }
    val db = RockyDatabase.local(root, shopping)
    try {
        for (table in listOf("lists", "categories", "items")) {
            db.credentials.installPseudonym(db.credentials.assignPseudonym(db.credentials.pseudonymRequest(table)))
        }
        val list = db.partitions.next("lists").getValue("id").jsonPrimitive.content.toLong()
        val category = db.write {
            check(nextId("lists") == list)
            execute("INSERT INTO lists(id, title) VALUES(?, ?)", list, "Groceries")
            nextId("categories", list).also { execute("INSERT INTO categories(id, list, name) VALUES(?, ?, ?)", it, list, "Dairy") }
        }
        db.write { execute("INSERT INTO items(id, list, key, title, category) VALUES(?, ?, ?, ?, ?)", nextId("items", list), list, "k1", "Milk", category) }
        check(runCatching { db.write { execute("INSERT INTO items(id, list, key, title) VALUES(?, ?, ?, ?)", nextId("items", list), list, "k2", "Milk") } }.isFailure)
        db.write { execute("DELETE FROM categories WHERE list = ? AND id = ?", list, category) }
        val items = Row.decode(
            db.read(
                query(
                    "SELECT id, title, category FROM items WHERE list = ?",
                    JsonArray(
                        listOf(
                            buildJsonObject {
                                put("type", "integer")
                                put("value", list.toString())
                            },
                        ),
                    ),
                ),
            ),
        )
        check(items.single().isNull("category") && items.single().text("title") == "Milk")
    } finally {
        db.close()
    }
}

/** Runs with only this consumer, packaged jars and a JVM: no repository, native paths or build tools. */
fun main(args: Array<String>) = runBlocking {
    val root = File(args.single()).also { it.mkdirs() }
    val directory = File(root, "database")
    val coordinator = DevelopmentCoordinator()
    var calls = 0
    val runtime = CommandRuntime(
        coordinator,
        listOf(
            add handledBy Handler { _, n ->
                calls++
                Decision.accept(n)
            },
        ),
    )
    var db = RockyDatabase.local(directory, schema, commands = runtime)
    check(!db.network().attached)
    val a = db.openSession()
    val b = db.openSession()
    val identity = db.commandIdentity()
    coordinator.register(identity)
    val node = a.credentials.status().node
    check(a.credentials.status().isIssuer)
    val initial = a.readiness().first()
    val revision = initial.getValue("revision").jsonPrimitive.long
    val changed = async(start = CoroutineStart.UNDISPATCHED) {
        a.readiness().first { it.getValue("revision").jsonPrimitive.long > revision }
    }
    // The issuer assigns its own author; inserts are signed under it.
    a.credentials.installPseudonym(a.credentials.assignPseudonym(a.credentials.pseudonymRequest("items")))
    val action = handle(1)
    suspend fun apples() = b.write(action) { execute("INSERT INTO items(id, title) VALUES(?, ?)", nextId("items"), "apples") }
    apples()
    check(withTimeout(5000) { changed.await() }.getValue("ready").jsonPrimitive.boolean)
    // Lost foreign acknowledgment: reconcile the handle rather than writing again. Another transaction under it is refused.
    check(b.reconcile(action).getValue("committed").jsonPrimitive.boolean)
    check(runCatching { apples() }.exceptionOrNull() is RockyException)
    check(titles(b) == listOf("apples"))
    val waiting = async(start = CoroutineStart.UNDISPATCHED) { a.readiness().first { false } }
    waiting.cancelAndJoin()
    a.close()
    check(titles(b) == listOf("apples"))
    b.commands.submit(add, 7L, handle(2))
    val ticket = b.commands.run(add, handle(2))
    check(ticket.state == TicketState.Applied && (ticket.outcome as Outcome.Accepted).value == 7L)
    check(calls == 1)
    b.commands.submit(add, 9L, handle(3))
    check(b.commands.cancel(add, handle(3)).state == TicketState.Cancelled)
    val endpoint = RockyEndpoint.bind(ByteArray(32) { 42 }, relay = "disabled", discovery = false, bind = "127.0.0.1:0")
    val attached = db.attach(PeerTransport.Endpoint(endpoint))
    check(db.network().attached && !db.network().owned)
    attached.close()
    check(!db.network().attached)
    check(endpoint.address().isNotEmpty())
    val owned = db.attach(PeerTransport.Tcp("127.0.0.1:0"))
    val invitation = db.credentials.pairing(GrantProfile.Writer(setOf("items")))
    val archive = File(root, "device.backup")
    db.backup(archive)
    val restored = RockyDatabase.restore(
        File(root, "restored"),
        schema,
        archive,
        invitation.uri,
        PeerTransport.Tcp("127.0.0.1:0"),
        commands = runtime,
    )
    check(restored.credentials.status().node != db.credentials.status().node)
    check(titles(restored) == listOf("apples"))
    check(restored.commands.ticket(add, handle(2))?.state == TicketState.Applied)
    check(calls == 1) // Restore and replay never redispatch the command handler.
    restored.close()
    owned.close()
    db.close()
    check(endpoint.address().isNotEmpty())
    endpoint.shutdown()
    // Reopen under the same identity and upgrade additively without reauthoring the action.
    val upgraded = schema.copy(version = 2, tables = listOf(Table("items", listOf(Column.text("title"), Column.text("note", nullable = true)), primaryKey = listOf("id"))))
    db = RockyDatabase.open(directory, upgraded, commands = runtime)
    check(db.credentials.status().node == node)
    check(db.generation().getValue("current").jsonObject.getValue("schema").jsonPrimitive.int == 2)
    check(titles(db) == listOf("apples"))
    check(db.commands.ticket(add, handle(2))?.state == TicketState.Applied)
    check(db.reconcile(action).getValue("committed").jsonPrimitive.boolean)
    check(calls == 1)
    db.close()
    // Cancellation during native construction still closes the admitted database and leaves it reopenable.
    val began = CountDownLatch(1)
    val proceed = CountDownLatch(1)
    val firstStore = AtomicBoolean(true)
    val keys = ConcurrentHashMap<String, ByteArray>()
    val store = object : SecretStore {
        override fun load(name: String): ByteArray? = keys[name]
        override fun store(name: String, secret: ByteArray) {
            if (firstStore.compareAndSet(true, false)) {
                began.countDown()
                proceed.await()
            }
            keys[name] = secret
        }
        override fun delete(name: String) {
            keys.remove(name)
        }
    }
    val cancelledDirectory = File(root, "cancelled-open")
    val opening = launch { RockyDatabase.local(cancelledDirectory, schema, secrets = store).close() }
    withContext(Dispatchers.IO) { check(began.await(10, java.util.concurrent.TimeUnit.SECONDS)) }
    opening.cancel()
    proceed.countDown()
    withTimeout(10000) { opening.join() }
    check(opening.isCancelled)
    val reopened = RockyDatabase.open(cancelledDirectory, schema, secrets = store)
    check(reopened.credentials.status().isIssuer)
    reopened.close()
    credentialPrimitives(File(root, "primitives"))
    val moveSchema = schema("packagedmoves") {
        table("rows") {
            text("list")
            text("title")
            primaryKey("list", "id")
            partitionBy("list")
        }
    }
    val moving = RockyDatabase.local(File(root, "moves"), moveSchema)
    try {
        for (partition in listOf("A", "B")) moving.partitions.create("rows", JsonArray(listOf(text(partition))))
        moving.credentials.installPseudonym(moving.credentials.assignPseudonym(moving.credentials.pseudonymRequest("rows")))
        var id = 0L
        moving.write {
            id = nextId("rows", "A")
            execute("INSERT INTO rows(list,title,id) VALUES(?,?,?)", "A", "kept", id)
        }
        moving.moveRow(handle(90), "rows", id, listOf("A"), listOf("B"))
        moving.moveRow(handle(90), "rows", id, listOf("A"), listOf("B"))
        val moved = Row.decode(moving.read(moveSchema.table("rows").query())).single()
        check(moved.text("list") == "B" && moved.integer("id") == id)
    } finally {
        moving.close()
    }
    groupedLists(File(root, "lists"))
    println("Packaged consumer passed: grouped partitioned constraints, idempotent row moves, credential primitives, non-issuer executor, local sessions, authority/progress Flow, reconciliation, Kotlin handler/tickets, owned/borrowed networks, fresh restore and generation reopen")
}
