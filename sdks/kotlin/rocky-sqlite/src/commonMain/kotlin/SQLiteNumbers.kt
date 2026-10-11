package rocky.sqlite

/** Rounds the exact binary value to SQLite's 15 decimal digits on every target. */
internal fun sqliteRealText(value: Double): String {
    val bits = value.toBits()
    val binaryExponent = ((bits ushr 52) and 2047).toInt()
    var mantissa = bits and ((1L shl 52) - 1)
    if (binaryExponent != 0) mantissa = mantissa or (1L shl 52)
    val exponent = (if (binaryExponent == 0) 1 else binaryExponent) - 1075
    val limbs = mutableListOf(mantissa % 1_000_000_000, mantissa / 1_000_000_000)
    repeat(kotlin.math.abs(exponent)) {
        var carry = 0L
        for (index in limbs.indices) {
            val product = limbs[index] * (if (exponent < 0) 5 else 2) + carry
            limbs[index] = product % 1_000_000_000
            carry = product / 1_000_000_000
        }
        if (carry != 0L) limbs.add(carry)
    }
    while (limbs.size > 1 && limbs.last() == 0L) limbs.removeAt(limbs.lastIndex)
    val exact = limbs.last().toString() + limbs.dropLast(1).asReversed().joinToString("") { it.toString().padStart(9, '0') }
    var power = exact.length - (if (exponent < 0) -exponent else 0) - 1
    var digits = exact.take(15)
    if (exact.length > 15 && exact[15] >= '5') {
        digits = (digits.toLong() + 1).toString()
        if (digits.length > 15) {
            power++
            digits = digits.take(15)
        }
    }
    digits = digits.trimEnd('0')
    val sign = if (value < 0) "-" else ""
    if (power < -4 || power >= 15) {
        val suffix = (if (power < 0) "-" else "+") + kotlin.math.abs(power).toString().padStart(2, '0')
        return "$sign${digits.first()}.${digits.drop(1).ifEmpty { "0" }}e$suffix"
    }
    val point = power + 1
    val plain = when {
        point <= 0 -> "0." + "0".repeat(-point) + digits
        point >= digits.length -> digits + "0".repeat(point - digits.length) + ".0"
        else -> digits.take(point) + "." + digits.drop(point)
    }
    return sign + plain
}
