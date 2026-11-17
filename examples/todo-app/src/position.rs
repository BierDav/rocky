//! Fractional indexes: a todo's place is a base-62 string between its
//! neighbours', so a move rewrites one row. Each key ends with its todo's
//! own row identity as a tag, so two devices placing todos into one gap
//! never collide and every list has one total order.
use rocky_sdk::RowIdentity;

/// Digits in ascending byte order, so SQLite's binary collation and Rust's
/// `str` ordering agree with digit order.
const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: u8 = 62;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PositionError {
    /// `before` does not sort strictly before `after`.
    Unordered,
    /// A stored key holds a non-digit or ends with the zero digit.
    Invalid(String),
}
impl std::fmt::Display for PositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unordered => write!(f, "neighbouring positions are out of order"),
            Self::Invalid(key) => write!(f, "invalid position {key:?}"),
        }
    }
}
impl std::error::Error for PositionError {}

/// The digits of a stored key. A valid key ends with a non-zero digit: a
/// trailing zero digit would leave no key directly below it.
fn digits(key: &str) -> Result<Vec<u8>, PositionError> {
    let invalid = || PositionError::Invalid(key.to_owned());
    let digits = key
        .bytes()
        .map(|byte| {
            DIGITS
                .iter()
                .position(|digit| *digit == byte)
                .map(|index| index as u8)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(invalid)?;
    match digits.last() {
        Some(0) | None => Err(invalid()),
        Some(_) => Ok(digits),
    }
}

/// The key of todo `tag` strictly between `before` and `after`; `None`
/// stands for the list's start or end.
pub fn between(
    before: Option<&str>,
    after: Option<&str>,
    tag: RowIdentity,
) -> Result<String, PositionError> {
    let low = before.map(digits).transpose()?.unwrap_or_default();
    let high = after.map(digits).transpose()?;
    if let (Some(before), Some(after)) = (before, after)
        && before >= after
    {
        return Err(PositionError::Unordered);
    }
    let mut key = middle(&low, high.as_deref());
    key.extend(tag_digits(&tag.to_bytes()));
    Ok(key
        .into_iter()
        .map(|digit| DIGITS[digit as usize] as char)
        .collect())
}

/// The base-62 digits of the big-endian number `tag`, least significant
/// first: a non-zero tag's last digit is non-zero.
fn tag_digits(tag: &[u8]) -> Vec<u8> {
    let mut number = tag.to_vec();
    let mut digits = Vec::new();
    while number.iter().any(|byte| *byte != 0) {
        let mut remainder = 0_u16;
        for byte in &mut number {
            let value = (remainder << 8) | u16::from(*byte);
            *byte = (value / u16::from(BASE)) as u8;
            remainder = value % u16::from(BASE);
        }
        digits.push(remainder as u8);
    }
    digits
}

/// Digits strictly between `low` and `high` that are not a prefix of
/// `high`, so any suffix keeps them below `high`. Needs `low < high`.
/// Ends within `low`'s length plus one, as past it the low digit is zero.
fn middle(low: &[u8], high: Option<&[u8]>) -> Vec<u8> {
    let mut key = Vec::new();
    let mut high = high;
    loop {
        let index = key.len();
        let low_digit = low.get(index).copied().unwrap_or(0);
        let high_digit = high.map_or(BASE, |high| high.get(index).copied().unwrap_or(0));
        if high_digit > low_digit + 1 {
            key.push(low_digit.midpoint(high_digit));
            return key;
        }
        key.push(low_digit);
        let now_below_high = high_digit == low_digit + 1;
        if now_below_high {
            high = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_sdk::WriterId;

    fn tag(sequence: u64) -> RowIdentity {
        RowIdentity::new(WriterId::from_bytes([7; 32]), sequence).unwrap()
    }

    #[test]
    fn keys_sort_between_their_neighbours() {
        let first = between(None, None, tag(1)).unwrap();
        let last = between(Some(&first), None, tag(2)).unwrap();
        let start = between(None, Some(&first), tag(3)).unwrap();
        let mut keys = vec![start.clone(), first.clone(), last.clone()];
        let mut low = first.clone();
        let mut high = last.clone();
        for sequence in 4..200 {
            let key = between(Some(&low), Some(&high), tag(sequence)).unwrap();
            assert!(
                low < key && key < high,
                "splitting the narrowest gap on alternating sides: {low} < {key} < {high}"
            );
            keys.push(key.clone());
            if sequence % 2 == 0 {
                low = key;
            } else {
                high = key;
            }
        }
        let mut below = start;
        for sequence in 200..260 {
            let key = between(None, Some(&below), tag(sequence)).unwrap();
            assert!(key < below);
            below = key;
        }
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), 199);
    }

    #[test]
    fn concurrent_placements_into_one_gap_stay_distinct_and_ordered() {
        let low = between(None, None, tag(7)).unwrap();
        let high = between(Some(&low), None, tag(8)).unwrap();
        let left = between(Some(&low), Some(&high), tag(1_000_001)).unwrap();
        let right = between(Some(&low), Some(&high), tag(2_000_002)).unwrap();
        assert_ne!(left, right);
        for key in [&left, &right] {
            assert!(low < *key && *key < high);
        }
        let (a, b) = if left < right {
            (&left, &right)
        } else {
            (&right, &left)
        };
        let inner = between(Some(a), Some(b), tag(9)).unwrap();
        assert!(
            *a < inner && inner < *b,
            "a later placement between the two concurrent ones still fits"
        );
    }

    #[test]
    fn refuses_invalid_input() {
        assert_eq!(
            between(Some("b"), Some("a"), tag(1)),
            Err(PositionError::Unordered)
        );
        assert_eq!(
            between(Some("a"), Some("a"), tag(1)),
            Err(PositionError::Unordered)
        );
        assert_eq!(
            between(None, Some("a0"), tag(1)),
            Err(PositionError::Invalid("a0".into()))
        );
        assert_eq!(
            between(None, Some("a-"), tag(1)),
            Err(PositionError::Invalid("a-".into()))
        );
    }
}
