//! Fractional indexes: a todo's place is a base-62 string between its
//! neighbours', so a move rewrites one row. Each key ends with its todo's
//! own tag, so two devices placing todos into one gap never collide and
//! every list has one total order.

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
    /// Tags come from positive todo ids.
    InvalidTag(i64),
}
impl std::fmt::Display for PositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unordered => write!(f, "neighbouring positions are out of order"),
            Self::Invalid(key) => write!(f, "invalid position {key:?}"),
            Self::InvalidTag(id) => write!(f, "todo id {id} cannot tag a position"),
        }
    }
}
impl std::error::Error for PositionError {}

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
    // A trailing zero digit would leave no key directly below it.
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
    tag: i64,
) -> Result<String, PositionError> {
    if tag <= 0 {
        return Err(PositionError::InvalidTag(tag));
    }
    let low = before.map(digits).transpose()?.unwrap_or_default();
    let high = after.map(digits).transpose()?;
    if let (Some(before), Some(after)) = (before, after)
        && before >= after
    {
        return Err(PositionError::Unordered);
    }
    let mut key = middle(&low, high.as_deref());
    // Least significant digit first: the last digit is non-zero.
    let mut tag = tag as u64;
    while tag > 0 {
        key.push((tag % u64::from(BASE)) as u8);
        tag /= u64::from(BASE);
    }
    Ok(key
        .into_iter()
        .map(|digit| DIGITS[digit as usize] as char)
        .collect())
}

/// Digits strictly between `low` and `high` that are not a prefix of
/// `high`, so any suffix keeps them below `high`. Needs `low < high`.
fn middle(low: &[u8], high: Option<&[u8]>) -> Vec<u8> {
    let mut key = Vec::new();
    let mut high = high;
    // Ends within `low`'s length plus one: past it the low digit is zero.
    loop {
        let index = key.len();
        let low_digit = low.get(index).copied().unwrap_or(0);
        // Past a digit below `high`'s, anything above `low` fits.
        let high_digit = high.map_or(BASE, |high| high.get(index).copied().unwrap_or(0));
        if high_digit > low_digit + 1 {
            key.push(low_digit.midpoint(high_digit));
            return key;
        }
        key.push(low_digit);
        if high_digit == low_digit + 1 {
            high = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_between_their_neighbours() {
        let first = between(None, None, 1).unwrap();
        let last = between(Some(&first), None, 2).unwrap();
        let start = between(None, Some(&first), 3).unwrap();
        let mut keys = vec![start.clone(), first.clone(), last.clone()];
        // Repeatedly split the narrowest gaps on both sides.
        let mut low = first.clone();
        let mut high = last.clone();
        for tag in 4..200 {
            let key = between(Some(&low), Some(&high), tag).unwrap();
            assert!(low < key && key < high, "{low} < {key} < {high}");
            keys.push(key.clone());
            if tag % 2 == 0 {
                low = key;
            } else {
                high = key;
            }
        }
        let mut below = start;
        for tag in 200..260 {
            let key = between(None, Some(&below), tag).unwrap();
            assert!(key < below);
            below = key;
        }
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), 199);
    }

    #[test]
    fn concurrent_placements_into_one_gap_stay_distinct_and_ordered() {
        let low = between(None, None, 7).unwrap();
        let high = between(Some(&low), None, 8).unwrap();
        let left = between(Some(&low), Some(&high), 1_000_001).unwrap();
        let right = between(Some(&low), Some(&high), 2_000_002).unwrap();
        assert_ne!(left, right);
        for key in [&left, &right] {
            assert!(low < *key && *key < high);
        }
        // A later placement between the two concurrent ones still fits.
        let (a, b) = if left < right {
            (&left, &right)
        } else {
            (&right, &left)
        };
        let inner = between(Some(a), Some(b), 9).unwrap();
        assert!(*a < inner && inner < *b);
    }

    #[test]
    fn refuses_invalid_input() {
        assert_eq!(
            between(Some("b"), Some("a"), 1),
            Err(PositionError::Unordered)
        );
        assert_eq!(
            between(Some("a"), Some("a"), 1),
            Err(PositionError::Unordered)
        );
        assert_eq!(
            between(None, Some("a0"), 1),
            Err(PositionError::Invalid("a0".into()))
        );
        assert_eq!(
            between(None, Some("a-"), 1),
            Err(PositionError::Invalid("a-".into()))
        );
        assert_eq!(between(None, None, 0), Err(PositionError::InvalidTag(0)));
    }
}
