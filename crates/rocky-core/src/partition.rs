//! Canonical partition addresses, independent of credentials and storage.
use crate::{
    Affinity, Collation, Commitment, KeyColumn, ResourceScope, Scope, SqlValue, commitment,
    key_components, key_values,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum encoded tuple size, including column identities.
pub const MAX_PARTITION_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionError(pub &'static str);
impl std::fmt::Display for PartitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for PartitionError {}

/// A SQL-equality-normalized, non-null component of a partition tuple.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionValue(Vec<u8>);
impl PartitionValue {
    pub fn new(column: KeyColumn, value: SqlValue) -> Result<Self, PartitionError> {
        let bytes = key_components(&[column], &[value])
            .map_err(|_| PartitionError("invalid partition value"))?;
        let value = Self(bytes);
        value.validate()?;
        Ok(value)
    }
    pub fn value(&self) -> Result<SqlValue, PartitionError> {
        self.validate()?;
        key_values(&self.0)
            .map_err(|_| PartitionError("invalid partition value"))?
            .into_iter()
            .next()
            .ok_or(PartitionError("missing partition value"))
    }
    pub fn validate(&self) -> Result<(), PartitionError> {
        let invalid = PartitionError("noncanonical partition value");
        if self.0.len() > MAX_PARTITION_BYTES || self.0.first() != Some(&1) {
            return Err(invalid);
        }
        let values = key_values(&self.0).map_err(|_| invalid.clone())?;
        let affinity = match values.first() {
            Some(SqlValue::Integer(_)) => Affinity::Integer,
            Some(SqlValue::Real(_)) => Affinity::Real,
            Some(SqlValue::Text(_)) => Affinity::Text,
            Some(SqlValue::Blob(_)) => Affinity::Blob,
            _ => return Err(invalid),
        };
        let collation = match self.0.get(2) {
            Some(0) => Collation::Binary,
            Some(1) => Collation::Nocase,
            Some(2) => Collation::Rtrim,
            _ => return Err(invalid),
        };
        let encoded = key_components(
            &[KeyColumn {
                affinity,
                collation,
            }],
            &values,
        )
        .map_err(|_| invalid.clone())?;
        if encoded != self.0 {
            return Err(invalid);
        }
        Ok(())
    }
}

/// Stable column identities paired with their canonical values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionTuple(#[serde(with = "components")] BTreeMap<Commitment, PartitionValue>);
impl PartitionTuple {
    pub fn new(
        values: impl IntoIterator<Item = (Commitment, PartitionValue)>,
    ) -> Result<Self, PartitionError> {
        let mut components = BTreeMap::new();
        for (column, value) in values {
            if components.insert(column, value).is_some() {
                return Err(PartitionError("duplicate partition column"));
            }
        }
        let tuple = Self(components);
        tuple.validate()?;
        Ok(tuple)
    }
    pub fn values(&self) -> &BTreeMap<Commitment, PartitionValue> {
        &self.0
    }
    pub fn validate(&self) -> Result<(), PartitionError> {
        if self.0.is_empty() || self.0.len() > 16 {
            return Err(PartitionError("a partition has 1 to 16 columns"));
        }
        let mut length = 1;
        for value in self.0.values() {
            value.validate()?;
            length += 32 + 4 + value.0.len();
        }
        if length > MAX_PARTITION_BYTES {
            return Err(PartitionError("partition tuple quota"));
        }
        Ok(())
    }
    /// The encryption/log boundary of this tuple in one table and domain.
    pub fn scope(&self, domain: Scope, table: Commitment) -> Result<ResourceScope, PartitionError> {
        self.validate()?;
        let mut bytes = Vec::new();
        bytes.extend(domain.mesh.0);
        bytes.extend(domain.app.0);
        bytes.extend(domain.domain.0);
        bytes.extend(table);
        bytes.push(self.0.len() as u8);
        for (column, value) in &self.0 {
            bytes.extend(column);
            bytes.extend((value.0.len() as u32).to_be_bytes());
            bytes.extend(&value.0);
        }
        Ok(ResourceScope {
            domain,
            table,
            encryption: commitment("rocky.partition-scope.v1", &bytes),
        })
    }
}

/// A selector must name every partition column, even unrestricted ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartitionSelection {
    Any,
    Only(BTreeSet<PartitionValue>),
}
impl PartitionSelection {
    fn includes(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Any, _) => true,
            (Self::Only(a), Self::Only(b)) => b.is_subset(a),
            _ => false,
        }
    }
    fn contains(&self, value: &PartitionValue) -> bool {
        match self {
            Self::Any => true,
            Self::Only(values) => values.contains(value),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionSelector(
    #[serde(with = "components")] pub BTreeMap<Commitment, PartitionSelection>,
);
impl PartitionSelector {
    pub fn validate(&self) -> Result<(), PartitionError> {
        if self.0.is_empty() || self.0.len() > 16 {
            return Err(PartitionError("a selector has 1 to 16 columns"));
        }
        let mut bytes = 0;
        for selection in self.0.values() {
            if let PartitionSelection::Only(values) = selection {
                if values.is_empty() || values.len() > 256 {
                    return Err(PartitionError("a finite selector has 1 to 256 values"));
                }
                for value in values {
                    value.validate()?;
                    bytes += value.0.len();
                }
            }
        }
        if bytes > 16 * MAX_PARTITION_BYTES {
            return Err(PartitionError("partition selector quota"));
        }
        Ok(())
    }
    pub fn includes(&self, other: &Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && self.0.len() == other.0.len()
            && other.0.iter().all(|(column, selection)| {
                self.0
                    .get(column)
                    .is_some_and(|held| held.includes(selection))
            })
    }
    pub fn matches(&self, tuple: &PartitionTuple) -> bool {
        self.validate().is_ok()
            && tuple.validate().is_ok()
            && self.0.len() == tuple.0.len()
            && tuple.0.iter().all(|(column, value)| {
                self.0
                    .get(column)
                    .is_some_and(|selection| selection.contains(value))
            })
    }
}

// Pairs also cross JSON without map keys or lossy integer conversions.
mod components {
    use super::*;
    use serde::{Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer, V: Serialize>(
        values: &BTreeMap<Commitment, V>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        values.iter().collect::<Vec<_>>().serialize(to)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, V: Deserialize<'de>>(
        from: D,
    ) -> Result<BTreeMap<Commitment, V>, D::Error> {
        let pairs = Vec::<(Commitment, V)>::deserialize(from)?;
        if pairs.is_empty() || pairs.len() > 16 {
            return Err(D::Error::custom("a partition has 1 to 16 columns"));
        }
        let mut values = BTreeMap::new();
        for (column, value) in pairs {
            if values.insert(column, value).is_some() {
                return Err(D::Error::custom("duplicate partition column"));
            }
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppId, DomainId, MeshId};
    fn text(value: &str) -> PartitionValue {
        PartitionValue::new(
            KeyColumn {
                affinity: Affinity::Text,
                collation: Collation::Nocase,
            },
            SqlValue::Text(value.into()),
        )
        .unwrap()
    }
    #[test]
    fn sql_equal_values_share_scope_but_domains_tables_and_columns_do_not() {
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([3; 16]),
        };
        let a = PartitionTuple::new([([4; 32], text("CUSTOMER"))]).unwrap();
        let b = PartitionTuple::new([([4; 32], text("customer"))]).unwrap();
        assert_eq!(a.scope(domain, [5; 32]), b.scope(domain, [5; 32]));
        assert_eq!(
            a.scope(domain, [5; 32]).unwrap().encryption,
            [
                207, 220, 251, 171, 155, 35, 80, 204, 168, 76, 114, 152, 197, 234, 75, 109, 188,
                89, 233, 81, 144, 216, 93, 81, 160, 126, 121, 176, 104, 150, 38, 227
            ]
        );
        assert_ne!(a.scope(domain, [5; 32]), a.scope(domain, [6; 32]));
        assert_ne!(
            a.scope(domain, [5; 32]),
            a.scope(
                Scope {
                    domain: DomainId([7; 16]),
                    ..domain
                },
                [5; 32]
            )
        );
        let c = PartitionTuple::new([([8; 32], text("customer"))]).unwrap();
        assert_ne!(a.scope(domain, [5; 32]), c.scope(domain, [5; 32]));
        assert!(PartitionTuple::new([([4; 32], text("a")), ([4; 32], text("b"))]).is_err());
    }
    #[test]
    fn selectors_narrow_without_losing_columns() {
        let all = PartitionSelector(
            [
                ([1; 32], PartitionSelection::Any),
                ([2; 32], PartitionSelection::Any),
            ]
            .into(),
        );
        let customer = PartitionSelector(
            [
                ([1; 32], PartitionSelection::Only([text("x")].into())),
                ([2; 32], PartitionSelection::Any),
            ]
            .into(),
        );
        let tuple = PartitionTuple::new([([1; 32], text("X")), ([2; 32], text("w"))]).unwrap();
        assert!(all.includes(&customer));
        assert!(!customer.includes(&all));
        assert!(customer.matches(&tuple));
        let missing = PartitionSelector([([1; 32], PartitionSelection::Any)].into());
        assert!(!all.includes(&missing));
        assert!(!missing.matches(&tuple));
        let empty =
            PartitionSelector([([1; 32], PartitionSelection::Only(BTreeSet::new()))].into());
        assert!(!all.includes(&empty));
    }
    #[test]
    fn tuples_and_selectors_cross_json_without_map_keys_or_lossy_numbers() {
        let integer = PartitionValue::new(
            KeyColumn {
                affinity: Affinity::Integer,
                collation: Collation::Binary,
            },
            SqlValue::Integer(i64::MAX),
        )
        .unwrap();
        let tuple = PartitionTuple::new([([1; 32], integer)]).unwrap();
        let encoded = serde_json::to_vec(&tuple).unwrap();
        assert_eq!(
            serde_json::from_slice::<PartitionTuple>(&encoded).unwrap(),
            tuple
        );
        let selector = PartitionSelector([([1; 32], PartitionSelection::Any)].into());
        let encoded = serde_json::to_vec(&selector).unwrap();
        assert_eq!(
            serde_json::from_slice::<PartitionSelector>(&encoded).unwrap(),
            selector
        );
        let duplicate =
            serde_json::to_vec(&vec![([1; 32], text("a")), ([1; 32], text("b"))]).unwrap();
        assert!(serde_json::from_slice::<PartitionTuple>(&duplicate).is_err());
    }
    #[test]
    fn decoded_components_are_checked_before_use() {
        for bytes in [
            vec![],
            vec![0],
            vec![2, 1, 0],
            vec![1, 1, 1, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        ] {
            assert!(PartitionValue(bytes).validate().is_err());
        }
        let mut bytes = text("x").0;
        bytes[7] = b'X';
        assert!(PartitionValue(bytes).validate().is_err());
        let huge = PartitionValue::new(
            KeyColumn {
                affinity: Affinity::Blob,
                collation: Collation::Binary,
            },
            SqlValue::Blob(vec![0; 1000]),
        )
        .unwrap();
        assert!(PartitionTuple::new([([1; 32], huge)]).is_err());
    }
}
