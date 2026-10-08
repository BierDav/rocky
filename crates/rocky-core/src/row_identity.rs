//! Visible incarnation numbers. Format: `docs/runtime/logical-schema.md`.
use std::fmt;

/// A positive 63-bit incarnation identity, with no timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowIdentity(i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowIdentityError {
    Author,
    Sequence,
    Exhausted,
    Representation,
}
impl fmt::Display for RowIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Author => "row author must be between 1 and 2^31-1; author 0 is reserved",
            Self::Sequence => "row sequence must be between 1 and 2^32-1; id 0 is reserved",
            Self::Exhausted => "row identity sequence exhausted",
            Self::Representation => "row identity must be a canonical positive decimal string",
        })
    }
}
impl std::error::Error for RowIdentityError {}
impl RowIdentity {
    pub fn new(author: u32, sequence: u32) -> Result<Self, RowIdentityError> {
        if author == 0 || author > 0x7fff_ffff {
            return Err(RowIdentityError::Author);
        }
        if sequence == 0 {
            return Err(RowIdentityError::Sequence);
        }
        Ok(Self((i64::from(author) << 32) | i64::from(sequence)))
    }
    /// Application identities exclude the reserved virtual row.
    pub fn from_integer(value: i64) -> Result<Self, RowIdentityError> {
        if value <= 0 {
            return Err(RowIdentityError::Sequence);
        }
        Self::new((value >> 32) as u32, value as u32)
    }
    pub fn integer(self) -> i64 {
        self.0
    }
    pub fn author(self) -> u32 {
        (self.0 >> 32) as u32
    }
    pub fn sequence(self) -> u32 {
        self.0 as u32
    }
    /// Continues a summarized creation log; zero means no prior creation.
    pub fn next(author: u32, highest: u32) -> Result<Self, RowIdentityError> {
        let sequence = highest.checked_add(1).ok_or(RowIdentityError::Exhausted)?;
        Self::new(author, sequence)
    }
}
impl fmt::Display for RowIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for RowIdentity {
    type Err = RowIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let integer = value
            .parse::<i64>()
            .map_err(|_| RowIdentityError::Representation)?;
        let identity = Self::from_integer(integer)?;
        if identity.to_string() != value {
            return Err(RowIdentityError::Representation);
        }
        Ok(identity)
    }
}
impl serde::Serialize for RowIdentity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> serde::Deserialize<'de> for RowIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_preserves_large_ids_as_canonical_strings() {
        let identity = RowIdentity::new(0x7fff_ffff, u32::MAX).unwrap();
        let json = serde_json::to_string(&identity).unwrap();
        assert_eq!(json, "\"9223372036854775807\"");
        assert_eq!(
            serde_json::from_str::<RowIdentity>(&json).unwrap(),
            identity
        );
        for invalid in [
            "9223372036854775807",
            "\"09223372036854775807\"",
            "\"+4294967297\"",
            "\"0\"",
            "\"4294967296\"",
            "\"1e10\"",
        ] {
            assert!(serde_json::from_str::<RowIdentity>(invalid).is_err());
        }
    }
    #[test]
    fn identities_have_exact_boundaries_and_never_wrap() {
        let first = RowIdentity::next(1, 0).unwrap();
        assert_eq!(first.integer(), 0x1_0000_0001);
        let last = RowIdentity::new(0x7fff_ffff, u32::MAX).unwrap();
        assert_eq!(last.integer(), i64::MAX);
        for identity in [first, last, RowIdentity::new(123, 456).unwrap()] {
            assert_eq!(
                RowIdentity::from_integer(identity.integer()).unwrap(),
                identity
            );
            assert_eq!(
                RowIdentity::new(identity.author(), identity.sequence()).unwrap(),
                identity
            );
        }
        assert_eq!(
            RowIdentity::next(1, u32::MAX),
            Err(RowIdentityError::Exhausted)
        );
        assert_eq!(RowIdentity::new(0, 1), Err(RowIdentityError::Author));
        assert_eq!(RowIdentity::new(1 << 31, 1), Err(RowIdentityError::Author));
        assert_eq!(RowIdentity::new(1, 0), Err(RowIdentityError::Sequence));
        for value in [0, -1, 1, 1_i64 << 32] {
            assert!(RowIdentity::from_integer(value).is_err());
        }
    }
}
