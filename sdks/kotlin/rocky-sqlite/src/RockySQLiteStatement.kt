package rocky.sqlite

import androidx.sqlite.SQLITE_DATA_BLOB
import androidx.sqlite.SQLITE_DATA_FLOAT
import androidx.sqlite.SQLITE_DATA_INTEGER
import androidx.sqlite.SQLITE_DATA_NULL
import androidx.sqlite.SQLITE_DATA_TEXT
import androidx.sqlite.SQLiteStatement
import androidx.sqlite.throwSQLiteException
import java.math.BigDecimal
import java.math.MathContext
import rocky.internal.native.NativePage
import rocky.internal.native.NativeStatement
import rocky.internal.native.NativeValue

/**
 * One statement. It runs at its first `step` or column query, once its parameters are bound, and then delivers
 * its result a page at a time; getters convert values as SQLite's column functions do.
 */
internal class RockySQLiteStatement(private val connection: RockySQLiteConnection, private val sql: String) : SQLiteStatement {
    private var native: NativeStatement? = null
    private val bindings = ArrayList<NativeValue>()
    private var page: NativePage? = null
    private var row = -1
    private var closed = false

    override fun bindBlob(index: Int, value: ByteArray) = bind(index, NativeValue.Blob(value.copyOf()))
    override fun bindDouble(index: Int, value: Double) = bind(index, NativeValue.Real(value))
    override fun bindLong(index: Int, value: Long) = bind(index, NativeValue.Integer(value))
    override fun bindText(index: Int, value: String) = bind(index, NativeValue.Text(value))
    override fun bindNull(index: Int) = bind(index, NativeValue.Null)
    private fun bind(index: Int, value: NativeValue) {
        open()
        if (index < 1 || index > MAX_PARAMETERS) throwSQLiteException(25, "column index out of range")
        while (bindings.size < index) bindings.add(NativeValue.Null)
        bindings[index - 1] = value
    }

    override fun step(): Boolean {
        val current = execute()
        row++
        var rows = current
        while (row >= rows.rows.size) {
            if (rows.done) {
                row = rows.rows.size
                return false
            }
            rows = native { statement().next() }
            page = rows
            row = 0
        }
        return true
    }
    override fun reset() {
        open()
        finish()
    }
    override fun clearBindings() {
        open()
        bindings.clear()
    }
    override fun close() {
        if (closed) return
        finish()
        closed = true
    }

    override fun getColumnCount(): Int = execute().columns.size
    override fun getColumnName(index: Int): String = execute().columns.getOrNull(index)
        ?: throwSQLiteException(25, "column index out of range")
    override fun getColumnType(index: Int): Int = when (value(index)) {
        is NativeValue.Null -> SQLITE_DATA_NULL
        is NativeValue.Integer -> SQLITE_DATA_INTEGER
        is NativeValue.Real -> SQLITE_DATA_FLOAT
        is NativeValue.Text -> SQLITE_DATA_TEXT
        is NativeValue.Blob -> SQLITE_DATA_BLOB
    }
    override fun isNull(index: Int): Boolean = value(index) is NativeValue.Null
    override fun getLong(index: Int): Long = when (val value = value(index)) {
        is NativeValue.Null -> 0
        is NativeValue.Integer -> value.value
        is NativeValue.Real -> integer(value.value)
        is NativeValue.Text -> integer(value.value)
        is NativeValue.Blob -> integer(value.value.decodeToString())
    }
    override fun getDouble(index: Int): Double = when (val value = value(index)) {
        is NativeValue.Null -> 0.0
        is NativeValue.Integer -> value.value.toDouble()
        is NativeValue.Real -> value.value
        is NativeValue.Text -> real(value.value)
        is NativeValue.Blob -> real(value.value.decodeToString())
    }
    override fun getText(index: Int): String = when (val value = value(index)) {
        is NativeValue.Null -> ""
        is NativeValue.Integer -> value.value.toString()
        is NativeValue.Real -> text(value.value)
        is NativeValue.Text -> value.value
        is NativeValue.Blob -> value.value.decodeToString()
    }
    override fun getBlob(index: Int): ByteArray = when (val value = value(index)) {
        is NativeValue.Null -> ByteArray(0)
        is NativeValue.Blob -> value.value.copyOf()
        else -> getText(index).encodeToByteArray()
    }

    private fun open() {
        if (closed) throwSQLiteException(21, "statement is closed")
    }
    private fun statement(): NativeStatement = native ?: connection.native().statement(sql).also { native = it }
    /** The current page, running the statement first if it has not run. */
    private fun execute(): NativePage {
        open()
        page?.let { return it }
        val params = bindings.toList()
        return native { statement().execute(params) }.also { page = it; row = -1 }
    }
    private fun value(index: Int): NativeValue {
        val current = page ?: throwSQLiteException(21, "no row; step the statement first")
        val values = current.rows.getOrNull(row) ?: throwSQLiteException(21, "no row; step the statement first")
        return values.getOrNull(index) ?: throwSQLiteException(25, "column index out of range")
    }
    /** Drops the undelivered rest of the result; the next step runs the statement again. */
    private fun finish() {
        val current = page ?: return
        page = null
        row = -1
        if (!current.done) native?.let { statement -> native { statement.finish() } }
    }

    companion object {
        private const val MAX_PARAMETERS = 32766

        /** SQLite's text-to-integer conversion: the leading integer digits, saturating. */
        internal fun integer(text: String): Long {
            val prefix = DIGITS.find(text.trimStart(' ', '\t', '\n', '\r', '\u000c'))?.value ?: return 0
            return prefix.toBigInteger().coerceIn(Long.MIN_VALUE.toBigInteger(), Long.MAX_VALUE.toBigInteger()).toLong()
        }
        /** SQLite's real-to-integer conversion: truncation, saturating at the 64-bit bounds. */
        internal fun integer(value: Double): Long = when {
            value.isNaN() -> 0
            value <= -9.223372036854775808E18 -> Long.MIN_VALUE
            value >= 9.223372036854775807E18 -> Long.MAX_VALUE
            else -> value.toLong()
        }
        internal fun real(text: String): Double =
            NUMBER.find(text.trimStart(' ', '\t', '\n', '\r', '\u000c'))?.value?.toDoubleOrNull() ?: 0.0
        /** SQLite renders a real with 15 significant digits and always a decimal point or exponent. */
        internal fun text(value: Double): String {
            if (value.isNaN()) return ""
            if (value.isInfinite()) return if (value > 0) "Inf" else "-Inf"
            if (value == 0.0) return "0.0"
            val rounded = BigDecimal(value).round(MathContext(15))
            val exponent = rounded.precision() - rounded.scale() - 1
            if (exponent < -4 || exponent >= 15) {
                val digits = rounded.unscaledValue().abs().toString().trimEnd('0').ifEmpty { "0" }
                val mantissa = digits.first() + "." + digits.drop(1).ifEmpty { "0" }
                val sign = if (rounded.signum() < 0) "-" else ""
                val power = (if (exponent < 0) "-" else "+") + kotlin.math.abs(exponent).toString().padStart(2, '0')
                return "$sign${mantissa}e$power"
            }
            val plain = rounded.stripTrailingZeros().toPlainString()
            return if ('.' in plain) plain else "$plain.0"
        }
        private val DIGITS = Regex("^[+-]?\\d+")
        private val NUMBER = Regex("^[+-]?(\\d+\\.?\\d*|\\.\\d+)([eE][+-]?\\d+)?")
    }
}
