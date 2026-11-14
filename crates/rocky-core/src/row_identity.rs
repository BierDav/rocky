//! Portable writer and row identities.
//!
//! A writer is named by its node's full 32-byte public key, so independent
//! devices never need a shared allocator. A row identity is that writer and
//! its monotonic creation sequence; its canonical form is 40 bytes, the key
//! followed by the big-endian sequence, so byte order is identity order.
use crate::SqlValue;
use std::fmt;

/// The bytes of a canonical row identity.
pub const ROW_IDENTITY_BYTES: usize = 40;
/// The highest creation sequence; SQLite stores sequences as signed integers.
pub const MAX_ROW_SEQUENCE: u64 = i64::MAX as u64;

/// A writer namespace: a node's full public key, never a shortened number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WriterId([u8; 32]);

/// A row's immutable incarnation: its creating writer and that writer's
/// creation sequence, with no timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowIdentity {
    writer: WriterId,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowIdentityError {
    Sequence,
    Exhausted,
    Representation,
}
impl fmt::Display for RowIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sequence => "row sequence must be between 1 and 2^63-1",
            Self::Exhausted => "row identity sequence exhausted",
            Self::Representation => {
                "row identity must be 40 canonical bytes: a writer key and its sequence"
            }
        })
    }
}
impl std::error::Error for RowIdentityError {}

impl WriterId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn from_slice(bytes: &[u8]) -> Result<Self, RowIdentityError> {
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| RowIdentityError::Representation)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl fmt::Display for WriterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(f, &self.0)
    }
}
impl fmt::Debug for WriterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WriterId({self})")
    }
}
impl std::str::FromStr for WriterId {
    type Err = RowIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_slice(&unhex(value)?)
    }
}

impl RowIdentity {
    pub fn new(writer: WriterId, sequence: u64) -> Result<Self, RowIdentityError> {
        if sequence == 0 || sequence > MAX_ROW_SEQUENCE {
            return Err(RowIdentityError::Sequence);
        }
        Ok(Self { writer, sequence })
    }
    /// Continues a summarized creation log; zero means no prior creation.
    pub fn next(writer: WriterId, highest: u64) -> Result<Self, RowIdentityError> {
        let sequence = highest
            .checked_add(1)
            .filter(|sequence| *sequence <= MAX_ROW_SEQUENCE)
            .ok_or(RowIdentityError::Exhausted)?;
        Self::new(writer, sequence)
    }
    pub fn writer(self) -> WriterId {
        self.writer
    }
    pub fn sequence(self) -> u64 {
        self.sequence
    }
    pub fn to_bytes(self) -> [u8; ROW_IDENTITY_BYTES] {
        let mut bytes = [0; ROW_IDENTITY_BYTES];
        bytes[..32].copy_from_slice(&self.writer.0);
        bytes[32..].copy_from_slice(&self.sequence.to_be_bytes());
        bytes
    }
    /// Decodes the canonical form, refusing every other length or sequence.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RowIdentityError> {
        if bytes.len() != ROW_IDENTITY_BYTES {
            return Err(RowIdentityError::Representation);
        }
        let (writer, sequence) = bytes.split_at(32);
        let sequence = u64::from_be_bytes(
            sequence
                .try_into()
                .map_err(|_| RowIdentityError::Representation)?,
        );
        Self::new(WriterId::from_slice(writer)?, sequence)
    }
    /// The SQL value of an `id` column: a 40-byte BLOB.
    pub fn sql(self) -> SqlValue {
        SqlValue::Blob(self.to_bytes().to_vec())
    }
    /// Reads an `id` column, refusing every other storage class.
    pub fn from_sql(value: &SqlValue) -> Result<Self, RowIdentityError> {
        match value {
            SqlValue::Blob(bytes) => Self::from_bytes(bytes),
            _ => Err(RowIdentityError::Representation),
        }
    }
}
impl From<RowIdentity> for SqlValue {
    fn from(identity: RowIdentity) -> Self {
        identity.sql()
    }
}
impl fmt::Display for RowIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(f, &self.to_bytes())
    }
}
impl std::str::FromStr for RowIdentity {
    type Err = RowIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_bytes(&unhex(value)?)
    }
}

fn hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    bytes.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
}
/// Lowercase hexadecimal only, so every value has one spelling.
fn unhex(value: &str) -> Result<Vec<u8>, RowIdentityError> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(RowIdentityError::Representation),
    };
    let (pairs, rest) = value.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return Err(RowIdentityError::Representation);
    }
    pairs
        .iter()
        .map(|[high, low]| Ok((digit(*high)? << 4) | digit(*low)?))
        .collect()
}

/// Text in human-readable formats, canonical bytes in binary ones.
macro_rules! serde_bytes_or_text {
    ($type:ty, $expecting:literal, $from:expr) => {
        impl serde::Serialize for $type {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() {
                    serializer.collect_str(self)
                } else {
                    serializer.serialize_bytes(&$from(self))
                }
            }
        }
        impl<'de> serde::Deserialize<'de> for $type {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $type;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str($expecting)
                    }
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<$type, E> {
                        value.parse().map_err(E::custom)
                    }
                    fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<$type, E> {
                        <$type>::try_from(value).map_err(E::custom)
                    }
                }
                if deserializer.is_human_readable() {
                    deserializer.deserialize_str(Visitor)
                } else {
                    deserializer.deserialize_bytes(Visitor)
                }
            }
        }
    };
}
impl TryFrom<&[u8]> for WriterId {
    type Error = RowIdentityError;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::from_slice(bytes)
    }
}
impl TryFrom<&[u8]> for RowIdentity {
    type Error = RowIdentityError;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(bytes)
    }
}
serde_bytes_or_text!(WriterId, "a 32-byte writer key", |id: &WriterId| id.0);
serde_bytes_or_text!(RowIdentity, "a 40-byte row identity", |id: &RowIdentity| id
    .to_bytes());

#[cfg(test)]
mod tests {
    use super::*;
    fn writer(byte: u8) -> WriterId {
        WriterId::from_bytes([byte; 32])
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn identities_round_trip_one_canonical_form() {
        let identity = RowIdentity::new(writer(0xab), MAX_ROW_SEQUENCE).unwrap();
        let bytes = identity.to_bytes();
        assert_eq!(&bytes[..32], &[0xab; 32]);
        assert_eq!(&bytes[32..], &MAX_ROW_SEQUENCE.to_be_bytes());
        assert_eq!(RowIdentity::from_bytes(&bytes).unwrap(), identity);
        assert_eq!(RowIdentity::from_sql(&identity.sql()).unwrap(), identity);
        let json = serde_json::to_string(&identity).unwrap();
        assert_eq!(
            json,
            format!("\"{}{}\"", "ab".repeat(32), "7fffffffffffffff")
        );
        assert_eq!(
            serde_json::from_str::<RowIdentity>(&json).unwrap(),
            identity
        );
        let text = identity.to_string();
        assert_eq!(text.parse::<RowIdentity>().unwrap(), identity);
        for invalid in [
            text.to_uppercase(),
            text[..78].to_string(),
            format!("{text}00"),
            format!("{}0000000000000000", "ab".repeat(32)),
        ] {
            assert!(invalid.parse::<RowIdentity>().is_err(), "{invalid}");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn old_integer_identities_are_refused() {
        // The former packed `i64` and its decimal text are not identities.
        assert!(RowIdentity::from_sql(&SqlValue::Integer(0x1_0000_0001)).is_err());
        assert!(RowIdentity::from_sql(&SqlValue::Text("4294967297".into())).is_err());
        assert!("4294967297".parse::<RowIdentity>().is_err());
        assert!(serde_json::from_str::<RowIdentity>("\"4294967297\"").is_err());
        assert!(RowIdentity::from_bytes(&0x1_0000_0001_i64.to_be_bytes()).is_err());
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn sequences_have_exact_boundaries_and_never_wrap() {
        let first = RowIdentity::next(writer(1), 0).unwrap();
        assert_eq!(first.sequence(), 1);
        assert_eq!(
            RowIdentity::next(writer(1), MAX_ROW_SEQUENCE),
            Err(RowIdentityError::Exhausted)
        );
        assert_eq!(
            RowIdentity::new(writer(1), 0),
            Err(RowIdentityError::Sequence)
        );
        assert_eq!(
            RowIdentity::new(writer(1), MAX_ROW_SEQUENCE + 1),
            Err(RowIdentityError::Sequence)
        );
        // Byte order is identity order: writer first, then sequence.
        let mut ids = [
            RowIdentity::new(writer(2), 1).unwrap(),
            RowIdentity::new(writer(1), 256).unwrap(),
            RowIdentity::new(writer(1), 2).unwrap(),
        ];
        let mut bytes = ids.map(RowIdentity::to_bytes);
        ids.sort();
        bytes.sort();
        assert_eq!(ids.map(RowIdentity::to_bytes), bytes);
    }
}
