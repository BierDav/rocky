package rocky

import rocky.internal.native.NativeEvent
import rocky.internal.native.NativeEventListener
import rocky.internal.native.NativeException
import rocky.internal.native.NativeLevel
import rocky.internal.native.droppedEvents
import rocky.internal.native.setEventListener

/** The most detailed level a diagnostics listener receives. */
enum class DiagnosticLevel(internal val native: NativeLevel) {
    ERROR(NativeLevel.Error),
    WARN(NativeLevel.Warn),
    INFO(NativeLevel.Info),
    DEBUG(NativeLevel.Debug),
    TRACE(NativeLevel.Trace),
}

/**
 * One of Rocky's spans or events, rendered. Fields are short hashes, counts, durations and error categories; they
 * never carry row values, keys, invitation codes or credentials. A closed span has [durationMillis] and lies inside
 * the spans [path] names, outermost first.
 */
data class DiagnosticEvent(
    val level: DiagnosticLevel,
    val atEpochMillis: Long,
    val path: String,
    val name: String,
    val fields: String,
    val durationMillis: Long?,
) {
    /** One line for a log file. */
    fun render(): String = buildString {
        append(level.name).append(' ')
        if (path.isNotEmpty()) append(path).append('/')
        append(name)
        if (fields.isNotEmpty()) append(' ').append(fields)
        durationMillis?.let { append(" duration_ms=").append(it) }
    }
}

/** Rocky's structured events, for applications that opt in. Nothing is collected until a listener is set. */
object RockyDiagnostics {
    /**
     * Delivers events up to [level] to [listener] on Rocky's own thread, never a database thread, replacing any
     * listener before; `null` stops delivery. Events the listener does not keep up with are dropped and counted in
     * [dropped].
     */
    fun listen(level: DiagnosticLevel, listener: ((DiagnosticEvent) -> Unit)?) {
        val native = listener?.let { deliver ->
            object : NativeEventListener {
                override fun event(event: NativeEvent) {
                    runCatching { deliver(event.common()) }
                }
            }
        }
        try {
            setEventListener(native, level.native)
        } catch (error: NativeException) {
            throw RockyException("rejected", false, error.message ?: "diagnostics listener")
        }
    }

    /** Events dropped because the listener fell behind, since the process started. */
    val dropped: Long get() = droppedEvents().toLong()
}

private fun NativeEvent.common() = DiagnosticEvent(
    level = DiagnosticLevel.entries.first { it.native == level },
    atEpochMillis = atEpochMillis.toLong(),
    path = path,
    name = name,
    fields = fields,
    durationMillis = durationMillis?.toLong(),
)
