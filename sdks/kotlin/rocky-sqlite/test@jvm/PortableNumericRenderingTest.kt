package rocky.sqlite

import java.math.BigDecimal
import java.math.MathContext
import kotlin.random.Random
import kotlin.test.*

class PortableNumericRenderingTest {
    @Test fun portableRoundingRetainsTheJvmDecimalContract() {
        val random = Random(17)
        val values = listOf(Double.MIN_VALUE, Double.MAX_VALUE, 1e-4, 1e-5, 1e14, 1e15, -0.1) +
            List(1_000) { Double.fromBits(random.nextLong()) }.filter { it.isFinite() && it != 0.0 }
        for (value in values) {
            val rounded = BigDecimal(value).round(MathContext(15))
            val exponent = rounded.precision() - rounded.scale() - 1
            val expected = if (exponent < -4 || exponent >= 15) {
                val digits = rounded.unscaledValue().abs().toString().trimEnd('0')
                val sign = if (value < 0) "-" else ""
                val power = (if (exponent < 0) "-" else "+") + kotlin.math.abs(exponent).toString().padStart(2, '0')
                "$sign${digits.first()}.${digits.drop(1).ifEmpty { "0" }}e$power"
            } else {
                rounded.stripTrailingZeros().toPlainString().let { if ('.' in it) it else "$it.0" }
            }
            assertEquals(expected, sqliteRealText(value), "bits=${value.toBits()}")
        }
    }
}
