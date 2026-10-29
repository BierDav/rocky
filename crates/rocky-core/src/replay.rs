//! Intents of replayed tables: what a transaction ran, not the rows it made.
//!
//! An intent frame carries one partition's statements of a transaction with
//! their bound values, the author's stamp and the base the author read.
//! Replicas holding the partition replay intents in stamp order and drop a
//! transaction whose replay fails, in every partition of its bundle.
use crate::{PartitionTuple, SecureStamp, SqlValue};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Statements one intent may carry.
pub const MAX_INTENT_STATEMENTS: usize = 256;

/// One statement as the author ran it in its partition, parameters bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentStatement {
    pub sql: String,
    #[serde(with = "values")]
    pub params: Vec<SqlValue>,
}

/// Parameters in `SqlValue`'s canonical encoding, one byte string each.
mod values {
    use super::*;

    pub(super) fn serialize<S: Serializer>(
        values: &[SqlValue],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let encoded: Vec<Vec<u8>> = values
            .iter()
            .map(|value| {
                let mut bytes = Vec::new();
                value.encode(&mut bytes);
                bytes
            })
            .collect();
        encoded.serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<SqlValue>, D::Error> {
        Vec::<Vec<u8>>::deserialize(deserializer)?
            .into_iter()
            .map(|bytes| {
                let mut rest = bytes.as_slice();
                let value = SqlValue::decode(&mut rest).map_err(serde::de::Error::custom)?;
                if !rest.is_empty() {
                    return Err(serde::de::Error::custom("trailing parameter bytes"));
                }
                Ok(value)
            })
            .collect()
    }
}

/// The last sequence of one author the intent's author had applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BaseMark {
    pub author: u32,
    pub sequence: u64,
}

/// One partition's part of a transaction. It carries no row images.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentFrame {
    /// Canonical replay order within the partition and tip.
    pub stamp: SecureStamp,
    /// The tip frontier the author read, by author.
    pub base: Vec<BaseMark>,
    /// `None` for an unpartitioned table.
    pub partition: Option<PartitionTuple>,
    pub statements: Vec<IntentStatement>,
}

impl IntentFrame {
    /// Refuses a frame no author produces: no statements, too many, or a
    /// base that is not strictly ordered by author.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.statements.is_empty() || self.statements.len() > MAX_INTENT_STATEMENTS {
            return Err("intent statement count");
        }
        if self
            .base
            .windows(2)
            .any(|pair| pair[0].author >= pair[1].author)
        {
            return Err("intent base order");
        }
        Ok(())
    }
}

/// Why replay dropped a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// A constraint failed on the replica's own replay.
    Constraint,
    /// The replayed effects are outside the author's grants.
    Admission,
    /// A statement no longer compiles against the tip's schema.
    Schema,
    /// The engine refused a statement for another reason.
    Engine,
}

/// What became of a transaction of replayed tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOutcome {
    Applied,
    /// `final` once a seal covers the transaction; until then a later
    /// arrival ordered before it may still change the decision.
    Dropped {
        reason: DropReason,
        #[serde(rename = "final")]
        sealed: bool,
    },
}

/// A replica's signed statement of how one bundle member replayed, written
/// into that member's own log so replicas without its key can follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    /// The bundle's transaction id.
    pub transaction: crate::Commitment,
    /// The member's index in the bundle's membership.
    pub member: u16,
    /// `None` when applied.
    pub dropped: Option<DropReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(statements: usize, base: &[(u32, u64)]) -> IntentFrame {
        IntentFrame {
            stamp: SecureStamp {
                clock: 1,
                author: 1,
                sequence: 1,
            },
            base: base
                .iter()
                .map(|&(author, sequence)| BaseMark { author, sequence })
                .collect(),
            partition: None,
            statements: (0..statements)
                .map(|_| IntentStatement {
                    sql: "UPDATE t SET x=x+?".into(),
                    params: vec![SqlValue::Integer(1)],
                })
                .collect(),
        }
    }

    #[test]
    fn frames_refuse_what_no_author_produces() {
        assert_eq!(frame(1, &[(1, 3), (2, 1)]).check(), Ok(()));
        assert_eq!(frame(0, &[]).check(), Err("intent statement count"));
        assert_eq!(
            frame(MAX_INTENT_STATEMENTS + 1, &[]).check(),
            Err("intent statement count")
        );
        assert_eq!(
            frame(1, &[(2, 1), (1, 3)]).check(),
            Err("intent base order")
        );
        assert_eq!(
            frame(1, &[(1, 1), (1, 3)]).check(),
            Err("intent base order")
        );
    }

    #[test]
    fn outcomes_name_final_drops() {
        let dropped = TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: true,
        };
        assert_eq!(
            serde_json::to_string(&dropped).unwrap(),
            r#"{"dropped":{"reason":"constraint","final":true}}"#
        );
    }
}
