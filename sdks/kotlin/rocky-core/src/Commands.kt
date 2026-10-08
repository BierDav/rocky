package rocky

import kotlinx.serialization.json.*

/** The scalar types of command fields. An integer is a `Long`, a real a `Double`, a blob a `ByteArray`. */
enum class FieldType(internal val wire: String) { Integer("integer"), Real("real"), Text("text"), Blob("blob"), Boolean("boolean") }
data class Field(val name: String, val type: FieldType, val nullable: Boolean = false)

/** The fields of an argument or result record, in order, and the Kotlin type they carry. */
class Shape<T>(val fields: List<Field>, private val write: (T) -> List<Any?>, private val read: (List<Any?>) -> T) {
    internal fun cells(value: T): JsonArray {
        val values = write(value)
        require(values.size == fields.size) { "a record holds one value per field" }
        return JsonArray(fields.zip(values).map { (field, value) -> cell(field, value) })
    }
    internal fun value(cells: JsonArray): T {
        require(cells.size == fields.size) { "a record holds one value per field" }
        return read(fields.zip(cells).map { (field, cell) -> plain(cell.jsonObject, field.type) })
    }
    companion object {
        /** A record without fields. */
        val none: Shape<Unit> = Shape(emptyList(), { emptyList() }, { })
    }
}

/**
 * One version of a typed strong command. `I`, `A` and `R` are its arguments, its accepted result and its
 * rejected result: the compiler refuses another command's types. Scopes are table names. Every command
 * of a schema declares the same `resources`: the tables one confirmed sequence orders.
 */
open class Command<I, A, R>(
    val name: String, val version: Int = 1,
    /** The declared identity of the handler that decides this version. */
    val handler: String,
    val arguments: Shape<I>, val accepted: Shape<A>, val rejected: Shape<R>,
    /** The table whose readers may see the arguments. */
    val invocation: String,
    /** The table whose readers may see the result. */
    val result: String,
    val resources: List<String>,
) {
    internal fun json(): JsonObject = buildJsonObject {
        put("name", name); put("version", version); put("handler", handler)
        put("arguments", fields(arguments)); put("accepted", fields(accepted)); put("rejected", fields(rejected))
        put("invocation", invocation); put("result", result); put("resources", JsonArray(resources.map(::JsonPrimitive)))
    }
    private fun fields(shape: Shape<*>) = JsonArray(shape.fields.map { field ->
        buildJsonObject { put("name", field.name); put("type", field.type.wire); put("nullable", field.nullable) }
    })
    /** The typed outcome of a ticket's `outcome` object. */
    internal fun outcome(value: JsonObject): Outcome<A, R>? = when (value.getValue("kind").jsonPrimitive.content) {
        "accepted" -> Outcome.Accepted(accepted.value(value.getValue("values").jsonArray))
        "rejected" -> Outcome.Rejected(rejected.value(value.getValue("values").jsonArray))
        else -> null
    }
}
/** The commands of a schema: every supported version of each. `handlerGeneration` changes when their semantics do. */
class CommandSet(val commands: List<Command<*, *, *>>, val handlerGeneration: Int = 1) {
    init { require(handlerGeneration > 0 && commands.map { it.name to it.version }.distinct().size == commands.size) }
    internal fun json(): JsonObject = buildJsonObject {
        put("format", 1); put("handler", handlerGeneration); put("commands", JsonArray(commands.map { it.json() }))
    }
}

/** The business result of a confirmed command. A rejection is confirmed like an acceptance and changes no rows. */
sealed interface Outcome<out A, out R> {
    data class Accepted<A>(val value: A) : Outcome<A, Nothing>
    data class Rejected<R>(val value: R) : Outcome<Nothing, R>
}
enum class TicketState {
    /** Stored on this device; nothing was sent or prepared. */
    Queued,
    Prepared,
    /** Sent or submitted without an established answer. Run the same ticket again; never submit a new one. */
    OutcomeUnknown,
    /** Confirmed by the coordinator; rows this device reads are not all applied yet. */
    Confirmed,
    Applied,
    /** Withdrawn before it left this device. */
    Cancelled,
    /** The coordinator retired its identity: it never executes again, and whether it was confirmed is unavailable there. */
    Retired,
    /** Its admission window closed unconfirmed: it was never executed. Submitting again is a new business request. */
    Expired,
}
/** A peer's acknowledgment that it holds, or also applied, a result's rows of `table`. */
data class Custody(val peer: String, val table: String?, val applied: Boolean)
/**
 * What this device knows of a submitted command. `outcome` is null until confirmation, and also when
 * `outcomeUnavailable`: confirmed, but this device cannot read the result's table. `error` is why the
 * last run did not finish; it is a protocol failure, never the business rejection.
 */
data class Ticket<A, R>(
    val handle: String, val state: TicketState, val sequence: Long?, val outcome: Outcome<A, R>?,
    val outcomeUnavailable: Boolean, val custody: List<Custody>, val error: RockyException? = null,
)

/** Immutable reads of declared command resources at one confirmed predecessor. */
interface CommandReads {
    /** Exact primary key in declared key order; null means the row is absent. */
    fun get(table: String, key: List<Any?>): List<Any?>?
    /** Ascending keys, inclusive lower and exclusive upper bound; limit is 1..1024. */
    fun scan(table: String, lower: List<Any?>?, upper: List<Any?>?, limit: Int): List<List<Any?>>
}
internal class JsonCommandReads(
    private val getRow: (String, String) -> String,
    private val scanRows: (String, String?, String?, Int) -> String,
) : CommandReads {
    private fun key(values: List<Any?>): String = JsonArray(values.map(::cell)).toString()
    private fun row(value: JsonElement): List<Any?> = value.jsonArray.map { plain(it.jsonObject, null) }
    override fun get(table: String, key: List<Any?>): List<Any?>? {
        val result = Json.parseToJsonElement(getRow(table, key(key)))
        return if (result == JsonNull) null else row(result)
    }
    override fun scan(table: String, lower: List<Any?>?, upper: List<Any?>?, limit: Int): List<List<Any?>> {
        require(limit in 1..1024) { "handler scan limit must be 1..1024" }
        return Json.parseToJsonElement(scanRows(table, lower?.let(::key), upper?.let(::key), limit)).jsonArray.map(::row)
    }
}
/** What a handler decides on: keyed reads of confirmed rows at one predecessor. */
class CommandContext internal constructor(
    val method: String, val version: Int, val requestNode: String, val requestSequence: Long, val caller: String,
    val predecessorSequence: Long, private val reads: CommandReads,
) {
    /** A row in declared column order, or null for an absent primary key. */
    fun get(table: String, vararg key: Any?): List<Any?>? = reads.get(table, key.toList())
    /** Full primary keys bound the range. Either bound may be omitted. */
    fun scan(table: String, lower: List<Any?>? = null, upper: List<Any?>? = null, limit: Int): List<List<Any?>> {
        require(limit in 1..1024) { "handler scan limit must be 1..1024" }
        return reads.scan(table, lower, upper, limit)
    }
}
sealed interface Write {
    /** Inserts or replaces the row with this primary key: every declared column in order, then `id` and `row_kind`. */
    data class Put(val table: String, val row: List<Any?>) : Write
    /** Deletes the row whose single-column primary key is `id`. */
    data class Delete(val table: String, val id: String) : Write
}
/** A job intent a confirmed command leaves for the application to dispatch; `table`'s readers may see it. */
class JobIntent(val table: String, val payload: ByteArray)
class Decision<out A, out R> private constructor(internal val outcome: Outcome<A, R>, internal val writes: List<Write>, internal val jobs: List<JobIntent>) {
    companion object {
        fun <A> accept(value: A, writes: List<Write> = emptyList(), jobs: List<JobIntent> = emptyList()): Decision<A, Nothing> =
            Decision(Outcome.Accepted(value), writes, jobs)
        /** A rejection writes nothing. */
        fun <R> reject(value: R): Decision<Nothing, R> = Decision(Outcome.Rejected(value), emptyList(), emptyList())
    }
}
/**
 * Decides one command from its arguments and the confirmed rows alone. It runs on a runtime thread, one call
 * at a time, within the deadline. Do not read clocks, random numbers, files or the network here: every
 * executor must reach the same decision. Throwing fails the call, which runs again; reject with `Decision.reject`.
 */
fun interface Handler<I, A, R> {
    fun handle(context: CommandContext, arguments: I): Decision<A, R>
}
/** A command and the handler that decides it. */
class Registration<I, A, R>(val command: Command<I, A, R>, val handler: Handler<I, A, R>) {
    internal fun decide(context: CommandContext, arguments: JsonArray): JsonObject {
        val decision = handler.handle(context, command.arguments.value(arguments))
        return buildJsonObject {
            when (val outcome = decision.outcome) {
                is Outcome.Accepted -> { put("outcome", "accepted"); put("result", command.accepted.cells(outcome.value)) }
                is Outcome.Rejected -> { put("outcome", "rejected"); put("result", command.rejected.cells(outcome.value)) }
            }
            put("writes", JsonArray(decision.writes.map { write -> buildJsonObject {
                when (write) {
                    is Write.Put -> { put("table", write.table); put("put", JsonArray(write.row.map(::cell))) }
                    is Write.Delete -> { put("table", write.table); put("delete", JsonArray(listOf(cell(write.id)))) }
                }
            } }))
            put("jobs", JsonArray(decision.jobs.map { job -> buildJsonObject { put("scope", job.table); put("payload", bytes(job.payload)) } }))
        }
    }
}
infix fun <I, A, R> Command<I, A, R>.handledBy(handler: Handler<I, A, R>): Registration<I, A, R> = Registration(this, handler)

/** Routes handler calls of the runtime to the registered handler of their command and version. */
class CommandDispatcher(registrations: List<Registration<*, *, *>>) {
    private val handlers = registrations.associateBy { it.command.name to it.command.version }
    init { require(handlers.size == registrations.size) { "one handler per command version" } }
    /** `input` is a handler call and the result a decision, both as JSON of the call contract. */
    fun handle(input: String, reads: CommandReads): String {
        val call = Json.parseToJsonElement(input).jsonObject
        check(call.getValue("version").jsonPrimitive.int == 2) { "unsupported handler call" }
        val method = call.text("method")
        val version = call.getValue("command_version").jsonPrimitive.int
        val registration = handlers[method to version] ?: error("no handler is registered for $method version $version")
        val request = call.getValue("request").jsonObject
        val context = CommandContext(method, version, request.text("node"), request.text("sequence").toLong(), call.text("caller"),
            call.getValue("predecessor").jsonObject.text("sequence").toLong(), reads)
        return registration.decide(context, call.getValue("arguments").jsonArray).toString()
    }
}

/** The coordinator an application composes for strong commands; on the JVM, a `DevelopmentCoordinator`. */
interface CommandCoordinator
/** What strong commands need besides a database: the coordinator and, on a device that executes, the handlers. */
class CommandRuntime(val coordinator: CommandCoordinator, val handlers: List<Registration<*, *, *>> = emptyList(), val handlerDeadlineMillis: Long? = null) {
    init { require(handlerDeadlineMillis == null || handlerDeadlineMillis > 0) }
}
/** A device as a coordinator names it: its node and its user, neither a credential. `executes`: it holds a handler. */
class CommandIdentity internal constructor(val node: String, val principal: String, val executes: Boolean, internal val json: String) {
    /** The identity as its device reported it, for the domain's owner on another device. It grants nothing. */
    val encoded: String get() = json
    companion object {
        fun decode(encoded: String): CommandIdentity = commandIdentity(Json.parseToJsonElement(encoded).jsonObject)
    }
}
/**
 * Confirmed rows of a command's table, as of the confirmed `sequence` this device holds results through.
 * Without `complete` they are withheld, not empty. A later confirmed sequence may exist elsewhere.
 */
data class ConfirmedRows(val complete: Boolean, val columns: List<String>, val rows: List<List<Any?>>, val sequence: Long = 0)
internal fun confirmedRows(value: JsonObject) = ConfirmedRows(value.getValue("complete").jsonPrimitive.boolean,
    value.getValue("columns").jsonArray.map { it.jsonPrimitive.content },
    value.getValue("rows").jsonArray.map { row -> row.jsonArray.map { plain(it.jsonObject, null) } },
    value.getValue("sequence").jsonPrimitive.content.toLong())
internal fun commandIdentity(value: JsonObject) = CommandIdentity(value.text("node"), value.text("principal"),
    value.getValue("handler").jsonPrimitive.boolean, value.toString())

/** Typed strong commands of a database. Submitting, reading and cancelling are local; `run` executes or delivers. */
interface CommandPort {
    /**
     * Stores the command and returns its ticket. Persist `handle` first and retry with it: the same handle and
     * arguments return the same ticket, other arguments fail with `conflict`. Nothing is sent.
     */
    suspend fun <I, A, R> submit(command: Command<I, A, R>, arguments: I, handle: String): Ticket<A, R>
    /** What this device knows of `handle`, or null if it never committed it. */
    suspend fun <I, A, R> ticket(command: Command<I, A, R>, handle: String): Ticket<A, R>?
    /** Withdraws a ticket that has not left this device. One that was sent keeps its state: confirmation cannot be undone. */
    suspend fun <I, A, R> cancel(command: Command<I, A, R>, handle: String): Ticket<A, R>
    /** Executes the ticket on this device if it is the domain's executor, otherwise delivers it to the executor. */
    suspend fun <I, A, R> run(command: Command<I, A, R>, handle: String): Ticket<A, R>
}

internal fun <A, R> Command<*, A, R>.ticket(value: JsonObject): Ticket<A, R> {
    val result = value["outcome"]?.takeIf { it !is JsonNull }?.jsonObject
    val failure = value["error"]?.takeIf { it !is JsonNull }?.jsonObject
    return Ticket(
        value.text("handle"),
        when (val state = value.text("state")) {
            "queued" -> TicketState.Queued
            "prepared" -> TicketState.Prepared
            "outcome_unknown" -> TicketState.OutcomeUnknown
            "confirmed" -> TicketState.Confirmed
            "applied" -> TicketState.Applied
            "cancelled" -> TicketState.Cancelled
            "retired" -> TicketState.Retired
            "expired" -> TicketState.Expired
            else -> error("unknown ticket state $state")
        },
        value.getValue("sequence").jsonPrimitive.contentOrNull?.toLong(),
        result?.let(::outcome),
        result?.text("kind") == "unavailable",
        value.getValue("custody").jsonArray.map { it.jsonObject }.map {
            Custody(it.text("peer"), it.getValue("table").jsonPrimitive.contentOrNull, it.getValue("applied").jsonPrimitive.boolean)
        },
        failure?.let { RockyException(it.text("code"), it.getValue("retryable").jsonPrimitive.boolean, it.text("message")) },
    )
}
internal fun <I> Command<I, *, *>.submission(arguments: I, handle: String): JsonObject = request("command_submit") {
    put("handle", handleBytes(handle)); put("method", name); put("version", version); put("arguments", this@submission.arguments.cells(arguments))
}
internal fun handleBytes(handle: String): JsonArray {
    require(handle.length == 64 && handle.all { it in "0123456789abcdef" }) { "handle must be 32 bytes of lowercase hex" }
    return JsonArray(handle.chunked(2).map { JsonPrimitive(it.toInt(16)) })
}
private fun JsonObject.text(name: String): String = getValue(name).jsonPrimitive.content

/** A value as a tagged cell of the call contract, typed by its field. */
private fun cell(field: Field, value: Any?): JsonObject {
    if (value == null) { require(field.nullable) { "${field.name} is not nullable" }; return cell(null) }
    val matches = when (field.type) {
        FieldType.Integer -> value is Long || value is Int
        FieldType.Real -> value is Double
        FieldType.Text -> value is String
        FieldType.Blob -> value is ByteArray
        FieldType.Boolean -> value is Boolean
    }
    require(matches) { "${field.name} holds another type" }
    return cell(value)
}
internal fun cell(value: Any?): JsonObject = buildJsonObject {
    when (value) {
        null -> put("type", "null")
        is Boolean -> { put("type", "integer"); put("value", if (value) "1" else "0") }
        is Int -> { put("type", "integer"); put("value", value.toString()) }
        is Long -> { put("type", "integer"); put("value", value.toString()) }
        is Double -> {
            require(!value.isNaN()) { "NaN is not a value" }
            // Negative zero has no canonical form.
            val bits = if (value == 0.0) 0L else value.toRawBits()
            put("type", "real"); put("value", bits.toULong().toString(16).padStart(16, '0'))
        }
        is String -> { put("type", "text"); put("value", value) }
        is ByteArray -> { put("type", "blob"); put("value", bytes(value)) }
        else -> throw IllegalArgumentException("unsupported value type")
    }
}
/** The Kotlin value of a cell. A boolean field reads its integer 0 or 1. */
private fun plain(cell: JsonObject, type: FieldType?): Any? = when (val tag = cell.getValue("type").jsonPrimitive.content) {
    "null" -> null
    "integer" -> cell.getValue("value").jsonPrimitive.content.toLong().let { if (type == FieldType.Boolean) it != 0L else it }
    "real" -> Double.fromBits(cell.getValue("value").jsonPrimitive.content.toULong(16).toLong())
    "text" -> cell.getValue("value").jsonPrimitive.content
    "blob" -> ByteArray(cell.getValue("value").jsonArray.size) { cell.getValue("value").jsonArray[it].jsonPrimitive.int.toByte() }
    else -> error("unknown value type $tag")
}
