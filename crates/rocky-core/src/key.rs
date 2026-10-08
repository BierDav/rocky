//! Canonical business keys. Byte equality of two encodings is SQLite's
//! equality of the keys under the declared affinities and collations.
use crate::{Affinity, Collation, Commitment, DomainId, SqlValue, ValueError};

/// Longest encoded key, components only.
pub const MAX_KEY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyColumn {
    pub affinity: Affinity,
    pub collation: Collation,
}

/// The components of a key: a count, then per component its storage class,
/// collation, `u32` length and bytes. `values` are in primary-key order.
pub fn key_components(columns: &[KeyColumn], values: &[SqlValue]) -> Result<Vec<u8>, ValueError> {
    if columns.len() != values.len() || columns.is_empty() || columns.len() > usize::from(u8::MAX) {
        return Err(ValueError::Arity);
    }
    let mut bytes = vec![columns.len() as u8];
    for (column, value) in columns.iter().zip(values) {
        let (class, collation, content): (u8, Collation, Vec<u8>) = match column
            .affinity
            .store(value.clone())?
        {
            SqlValue::Null => return Err(ValueError::Null),
            // Sign bit flipped, big-endian: byte order is numeric order.
            SqlValue::Integer(value) => (
                1,
                Collation::Binary,
                ((value as u64) ^ (1 << 63)).to_be_bytes().to_vec(),
            ),
            SqlValue::Real(value) => (2, Collation::Binary, value.bits().to_be_bytes().to_vec()),
            SqlValue::Text(text) => (3, column.collation, column.collation.fold(&text)),
            SqlValue::Blob(blob) => (4, Collation::Binary, blob),
        };
        bytes.push(class);
        bytes.push(collation as u8);
        bytes.extend((content.len() as u32).to_le_bytes());
        bytes.extend(content);
        if bytes.len() > MAX_KEY_BYTES {
            return Err(ValueError::KeyTooLong);
        }
    }
    Ok(bytes)
}
/// Values that compare equal to the key's under its columns' collations:
/// folded text stands for every spelling the collation calls equal.
pub fn key_values(components: &[u8]) -> Result<Vec<SqlValue>, ValueError> {
    let malformed = ValueError::Arity;
    let (count, mut rest) = components.split_first().ok_or(malformed)?;
    let mut values = Vec::new();
    for _ in 0..*count {
        let (header, tail) = rest.split_at_checked(6).ok_or(malformed)?;
        let length = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;
        let (content, tail) = tail.split_at_checked(length).ok_or(malformed)?;
        rest = tail;
        let number = || <[u8; 8]>::try_from(content).map(u64::from_be_bytes);
        values.push(match header[0] {
            1 => SqlValue::Integer((number().map_err(|_| malformed)? ^ (1 << 63)) as i64),
            2 => SqlValue::Real(
                crate::Real::from_bits(number().map_err(|_| malformed)?).ok_or(malformed)?,
            ),
            3 => SqlValue::Text(String::from_utf8(content.to_vec()).map_err(|_| malformed)?),
            4 => SqlValue::Blob(content.to_vec()),
            _ => return Err(malformed),
        });
    }
    if rest.is_empty() {
        Ok(values)
    } else {
        Err(malformed)
    }
}
/// The row identity a key names: the components bound to their domain and
/// table. Physical names and file positions are not part of it.
pub fn bound_key(domain: DomainId, table: &Commitment, components: &[u8]) -> Vec<u8> {
    let mut bytes = b"rocky.key.v1".to_vec();
    bytes.extend(domain.0);
    bytes.extend(table);
    bytes.extend(components);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(affinity: Affinity, collation: Collation) -> KeyColumn {
        KeyColumn {
            affinity,
            collation,
        }
    }
    fn text(collation: Collation, value: &str) -> Vec<u8> {
        key_components(
            &[column(Affinity::Text, collation)],
            &[SqlValue::Text(value.into())],
        )
        .unwrap()
    }
    #[test]
    fn components_carry_class_length_and_collation() {
        // Lengths keep ("a","bc") apart from ("ab","c").
        let pair = [column(Affinity::Text, Collation::Binary); 2];
        let key = |a: &str, b: &str| {
            key_components(&pair, &[SqlValue::Text(a.into()), SqlValue::Text(b.into())]).unwrap()
        };
        assert_ne!(key("a", "bc"), key("ab", "c"));
        assert_ne!(text(Collation::Binary, "A"), text(Collation::Binary, "a"));
        assert_eq!(
            text(Collation::Nocase, "ABC"),
            text(Collation::Nocase, "abc")
        );
        // NOCASE folds ASCII only, as SQLite does.
        assert_ne!(text(Collation::Nocase, "É"), text(Collation::Nocase, "é"));
        assert_eq!(text(Collation::Rtrim, "a  "), text(Collation::Rtrim, "a"));
        assert_ne!(text(Collation::Rtrim, " a"), text(Collation::Rtrim, "a"));
        // No Unicode normalization: composed and decomposed forms differ.
        assert_ne!(
            text(Collation::Binary, "\u{e9}"),
            text(Collation::Binary, "e\u{301}")
        );
        // The same bytes as TEXT and as BLOB are different keys.
        let blob = key_components(
            &[column(Affinity::Blob, Collation::Binary)],
            &[SqlValue::Blob(b"a".to_vec())],
        )
        .unwrap();
        assert_ne!(blob, text(Collation::Binary, "a"));
    }
    #[test]
    fn numeric_components_follow_affinity() {
        let integer = [column(Affinity::Integer, Collation::Binary)];
        let key = |value: SqlValue| key_components(&integer, &[value]);
        let ordered: Vec<_> = [i64::MIN, -1, 0, 1, i64::MAX]
            .into_iter()
            .map(|value| key(SqlValue::Integer(value)).unwrap())
            .collect();
        assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(key(SqlValue::real(7.0).unwrap()), key(SqlValue::Integer(7)));
        assert_eq!(
            key(SqlValue::real(7.5).unwrap()),
            Err(ValueError::Class(Affinity::Integer))
        );
        assert_eq!(key(SqlValue::Null), Err(ValueError::Null));
        assert_eq!(
            key(SqlValue::Text("7".into())),
            Err(ValueError::Class(Affinity::Integer))
        );
        let real = [column(Affinity::Real, Collation::Binary)];
        assert_eq!(
            key_components(&real, &[SqlValue::Integer(2)]),
            key_components(&real, &[SqlValue::real(2.0).unwrap()])
        );
        assert_eq!(
            key_components(&real, &[SqlValue::Integer(i64::MAX)]),
            Err(ValueError::Precision)
        );
    }
    #[test]
    fn keys_are_bounded_and_bound_to_domain_and_table() {
        let column = [column(Affinity::Blob, Collation::Binary)];
        assert_eq!(
            key_components(&column, &[SqlValue::Blob(vec![0; MAX_KEY_BYTES])]),
            Err(ValueError::KeyTooLong)
        );
        assert_eq!(key_components(&column, &[]), Err(ValueError::Arity));
        let key = key_components(&column, &[SqlValue::Blob(vec![1])]).unwrap();
        assert_eq!(key_values(&key), Ok(vec![SqlValue::Blob(vec![1])]));
        assert!(key_values(&key[..key.len() - 1]).is_err());
        let folded = key_values(&text(Collation::Nocase, "ABC")).unwrap();
        assert_eq!(folded, vec![SqlValue::Text("abc".into())]);
        let bound = bound_key(DomainId([1; 16]), &[2; 32], &key);
        assert_ne!(bound, bound_key(DomainId([3; 16]), &[2; 32], &key));
        assert_ne!(bound, bound_key(DomainId([1; 16]), &[4; 32], &key));
    }
}
