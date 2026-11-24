use crate::SqlValue;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const MAX_INTENT_STATEMENTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentStatement {
    pub sql: String,
    #[serde(with = "values")]
    pub params: Vec<SqlValue>,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BaseMark {
    pub author: crate::WriterId,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub base: Vec<BaseMark>,
    pub statements: Vec<IntentStatement>,
}

impl Intent {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Constraint,
    Admission,
    Restricted,
    Schema,
    Engine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOutcome {
    Applied {
        #[serde(rename = "final")]
        sealed: bool,
    },
    Dropped {
        reason: DropReason,
        #[serde(rename = "final")]
        sealed: bool,
    },
}

pub const MAX_OUTCOME_MEMBERS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub transaction: crate::Commitment,
    pub after_seal: Option<crate::Commitment>,
    pub replayed: Vec<(u16, Vec<BaseMark>)>,
    pub linked: Option<crate::Commitment>,
    pub dropped: Option<DropReason>,
    pub engine: String,
}

impl OutcomeRecord {
    pub fn check(&self) -> Result<(), &'static str> {
        if self.replayed.is_empty()
            || self.replayed.len() > MAX_OUTCOME_MEMBERS
            || self.replayed.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err("outcome members");
        }
        if self.replayed.iter().any(|(_, prefix)| {
            prefix.len() > MAX_OUTCOME_MEMBERS
                || prefix
                    .windows(2)
                    .any(|pair| pair[0].author >= pair[1].author)
        }) {
            return Err("outcome prefix");
        }
        if self.engine.is_empty() || self.engine.len() > 64 {
            return Err("outcome engine");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(statements: usize, base: &[(u8, u64)]) -> Intent {
        Intent {
            base: base
                .iter()
                .map(|&(author, sequence)| BaseMark {
                    author: crate::WriterId::from_bytes([author; 32]),
                    sequence,
                })
                .collect(),
            statements: (0..statements)
                .map(|_| IntentStatement {
                    sql: "UPDATE t SET x=x+?".into(),
                    params: vec![SqlValue::Integer(1)],
                })
                .collect(),
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn outcomes_name_finality() {
        let dropped = TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: true,
        };
        assert_eq!(
            serde_json::to_string(&dropped).unwrap(),
            r#"{"dropped":{"reason":"constraint","final":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&TransactionOutcome::Applied { sealed: false }).unwrap(),
            r#"{"applied":{"final":false}}"#
        );
    }
}
