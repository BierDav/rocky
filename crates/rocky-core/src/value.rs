//! Scalar values at the application/storage boundary. The byte codec and the
//! getter conversions are specified in `docs/runtime/logical-schema.md`.
use crate::ProtocolError;

/// A REAL in canonical form: never NaN, and negative zero is zero. SQLite
/// stores NaN as NULL, so a NaN is not a REAL value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Real(u64);
impl Real {
    pub fn new(value: f64) -> Option<Self> {
        if value.is_nan() {
            return None;
        }
        Some(Self(if value == 0.0 { 0 } else { value.to_bits() }))
    }
    /// Rejects NaN and negative zero, which have no canonical form.
    pub fn from_bits(bits: u64) -> Option<Self> {
        Self::new(f64::from_bits(bits)).filter(|real| real.0 == bits)
    }
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
    pub fn bits(self) -> u64 {
        self.0
    }
}
impl std::fmt::Debug for Real {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(f)
    }
}

/// Values at the compiled application/storage boundary; no engine dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(Real),
    Text(String),
    Blob(Vec<u8>),
}
impl SqlValue {
    /// A REAL value, or None for NaN.
    pub fn real(value: f64) -> Option<Self> {
        Real::new(value).map(Self::Real)
    }
    /// Canonical bytes: a tag, then a fixed-width little-endian number or a
    /// `u32` length and the content.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let content: &[u8] = match self {
            Self::Null => return out.push(0),
            Self::Integer(value) => {
                out.push(1);
                return out.extend(value.to_le_bytes());
            }
            Self::Real(value) => {
                out.push(2);
                return out.extend(value.bits().to_le_bytes());
            }
            Self::Text(text) => {
                out.push(3);
                text.as_bytes()
            }
            Self::Blob(bytes) => {
                out.push(4);
                bytes
            }
        };
        out.extend((content.len() as u32).to_le_bytes());
        out.extend(content);
    }
    /// Reads one value from the front of `bytes`.
    pub fn decode(bytes: &mut &[u8]) -> Result<Self, ProtocolError> {
        let mut take = |length: usize| {
            let (head, tail) = bytes
                .split_at_checked(length)
                .ok_or(ProtocolError::Truncated)?;
            *bytes = tail;
            Ok::<_, ProtocolError>(head)
        };
        let fixed = |bytes: &[u8]| {
            let mut number = [0; 8];
            number.copy_from_slice(bytes);
            number
        };
        Ok(match take(1)?[0] {
            0 => Self::Null,
            1 => Self::Integer(i64::from_le_bytes(fixed(take(8)?))),
            2 => Self::Real(
                Real::from_bits(u64::from_le_bytes(fixed(take(8)?)))
                    .ok_or(ProtocolError::InvalidValue)?,
            ),
            tag @ (3 | 4) => {
                let mut length = [0; 4];
                length.copy_from_slice(take(4)?);
                let content = take(u32::from_le_bytes(length) as usize)?.to_vec();
                if tag == 3 {
                    Self::Text(String::from_utf8(content).map_err(|_| ProtocolError::InvalidValue)?)
                } else {
                    Self::Blob(content)
                }
            }
            _ => return Err(ProtocolError::InvalidValue),
        })
    }
    /// `sqlite3_column_int64`: a REAL truncates toward zero and saturates;
    /// text and blobs yield their longest leading integer, saturating.
    pub fn to_integer(&self) -> i64 {
        match self {
            Self::Null => 0,
            Self::Integer(value) => *value,
            Self::Real(value) => value.get() as i64,
            Self::Text(text) => leading_integer(text.as_bytes()),
            Self::Blob(bytes) => leading_integer(bytes),
        }
    }
    /// `sqlite3_column_double`: text and blobs yield their longest leading
    /// decimal number, or zero.
    pub fn to_real(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Integer(value) => *value as f64,
            Self::Real(value) => value.get(),
            Self::Text(text) => leading_real(text.as_bytes()),
            Self::Blob(bytes) => leading_real(bytes),
        }
    }
    /// `sqlite3_column_text`: a REAL renders with 15 significant digits, or
    /// 17 when 15 would read back as another double, and always a fraction
    /// digit. Blob bytes that are not UTF-8 are replaced.
    pub fn to_text(&self) -> Option<String> {
        Some(match self {
            Self::Null => return None,
            Self::Integer(value) => value.to_string(),
            Self::Real(value) => real_text(value.get()),
            Self::Text(text) => text.clone(),
            Self::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        })
    }
    /// `sqlite3_column_blob`: numbers yield the bytes of their text form.
    pub fn to_blob(&self) -> Option<Vec<u8>> {
        match self {
            Self::Blob(bytes) => Some(bytes.clone()),
            other => other.to_text().map(String::into_bytes),
        }
    }
}

fn skip_space(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .unwrap_or(bytes.len());
    &bytes[start..]
}
fn leading_integer(bytes: &[u8]) -> i64 {
    let bytes = skip_space(bytes);
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    let mut value: i64 = 0;
    for digit in digits.iter().take_while(|byte| byte.is_ascii_digit()) {
        let digit = i64::from(digit - b'0');
        // Accumulate negatively: the negative range is one larger.
        value = match value.checked_mul(10).and_then(|v| v.checked_sub(digit)) {
            Some(value) => value,
            None => return if negative { i64::MIN } else { i64::MAX },
        };
    }
    if negative {
        value
    } else {
        value.checked_neg().unwrap_or(i64::MAX)
    }
}
fn leading_real(bytes: &[u8]) -> f64 {
    let bytes = skip_space(bytes);
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut end = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
    let whole = digits(end);
    end += whole;
    let mut fraction = 0;
    if bytes.get(end) == Some(&b'.') {
        fraction = digits(end + 1);
        end += 1 + fraction;
    }
    if whole + fraction == 0 {
        return 0.0;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(end + 1), Some(b'-' | b'+')));
        let exponent = digits(end + 1 + sign);
        if exponent > 0 {
            end += 1 + sign + exponent;
        }
    }
    std::str::from_utf8(&bytes[..end])
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0.0)
}
/// SQLite's rendering of a REAL: 15 significant digits when they read back
/// as the same double, otherwise 17, laid out as `%!.17g`. The 17 digits are
/// correctly rounded here; the engine's last digit can differ by one, and
/// it renders subnormals with 17 digits. Both read back as the same double.
fn real_text(value: f64) -> String {
    if value.is_infinite() {
        return if value > 0.0 { "Inf" } else { "-Inf" }.into();
    }
    if value == 0.0 {
        return "0.0".into();
    }
    let short = format!("{value:.14e}");
    let formatted = if short.parse() == Ok(value) {
        short
    } else {
        format!("{value:.16e}")
    };
    let Some((mantissa, exponent)) = formatted.split_once('e') else {
        return formatted;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if value < 0.0 { "-" } else { "" };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        digits => digits,
    };
    if !(-4..=16).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        let direction = if exponent < 0 { '-' } else { '+' };
        return format!(
            "{sign}{first}.{rest}e{direction}{:02}",
            exponent.unsigned_abs()
        );
    }
    if exponent < 0 {
        let zeros = "0".repeat((-exponent - 1) as usize);
        return format!("{sign}0.{zeros}{digits}");
    }
    let whole = exponent as usize + 1;
    let mut digits = digits.to_string();
    while digits.len() < whole {
        digits.push('0');
    }
    let (integer, fraction) = digits.split_at(whole);
    let fraction = if fraction.is_empty() { "0" } else { fraction };
    format!("{sign}{integer}.{fraction}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reals_are_canonical_and_values_round_trip() {
        assert!(Real::new(f64::NAN).is_none());
        assert_eq!(Real::new(-0.0), Real::new(0.0));
        assert!(Real::from_bits((-0.0f64).to_bits()).is_none());
        assert!(Real::from_bits(f64::NAN.to_bits()).is_none());
        let values = [
            SqlValue::Null,
            SqlValue::Integer(i64::MIN),
            SqlValue::Integer(i64::MAX),
            SqlValue::Real(Real::new(f64::NEG_INFINITY).unwrap()),
            SqlValue::Real(Real::new(0.1).unwrap()),
            SqlValue::Text("grüße\0".into()),
            SqlValue::Blob(vec![0, 255]),
        ];
        let mut bytes = Vec::new();
        for value in &values {
            value.encode(&mut bytes);
        }
        let mut rest = bytes.as_slice();
        for value in &values {
            assert_eq!(&SqlValue::decode(&mut rest).unwrap(), value);
        }
        assert!(rest.is_empty());
        for corrupt in [
            &[9][..],
            &[1, 0],
            &[3, 2, 0, 0, 0, 0xff, 0xfe],
            &[2, 0, 0, 0, 0, 0, 0, 0xf8, 0x7f],
        ] {
            assert!(SqlValue::decode(&mut &corrupt[..]).is_err());
        }
    }
    #[test]
    fn getters_convert_between_storage_classes() {
        let text = |value: &str| SqlValue::Text(value.into());
        assert_eq!(text(" 12abc").to_integer(), 12);
        assert_eq!(text("-9223372036854775809").to_integer(), i64::MIN);
        assert_eq!(text("9223372036854775808").to_integer(), i64::MAX);
        assert_eq!(text("1e3").to_integer(), 1);
        assert_eq!(SqlValue::real(-3.9).unwrap().to_integer(), -3);
        assert_eq!(SqlValue::real(1e300).unwrap().to_integer(), i64::MAX);
        assert_eq!(text("1e3x").to_real(), 1000.0);
        assert_eq!(text(".5e").to_real(), 0.5);
        assert_eq!(text("abc").to_real(), 0.0);
        let real = |value: f64| SqlValue::real(value).unwrap().to_text().unwrap();
        assert_eq!(real(1.0), "1.0");
        assert_eq!(real(0.1), "0.1");
        assert_eq!(real(-1234.5), "-1234.5");
        assert_eq!(real(1e16), "10000000000000000.0");
        assert_eq!(real(1e17), "1.0e+17");
        assert_eq!(real(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(real(123456789012345.0), "123456789012345.0");
        assert_eq!(real(1.5e-5), "1.5e-05");
        assert_eq!(real(0.0001), "0.0001");
        assert_eq!(real(f64::INFINITY), "Inf");
        assert_eq!(SqlValue::Integer(7).to_blob(), Some(b"7".to_vec()));
        assert_eq!(SqlValue::Null.to_text(), None);
    }
}
