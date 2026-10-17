//! Read-only current-header inspection; never used for protocol admission.
use super::*;
use serde::{Deserialize, Serialize, de, ser};
use std::fmt;

#[derive(Clone)]
pub(super) enum Cbor {
    Null,
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Self>),
    Map(Vec<(Self, Self)>),
}
impl Cbor {
    pub(super) fn json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(v) => json!(v),
            Self::Unsigned(v) => json!(v),
            Self::Signed(v) => json!(v),
            Self::Bytes(v) => json!({"hex": hex(v)}),
            Self::Text(v) => json!(v),
            Self::Array(v) => v.iter().map(Self::json).collect(),
            Self::Map(v) => v
                .iter()
                .map(|(k, v)| {
                    (
                        match k {
                            Self::Text(k) => k.clone(),
                            _ => k.json().to_string(),
                        },
                        v.json(),
                    )
                })
                .collect(),
        }
    }
    pub(super) fn array(&self) -> Result<&[Self], Failure> {
        match self {
            Self::Array(v) => Ok(v),
            _ => Err("header array".into()),
        }
    }
    pub(super) fn number(&self) -> Result<u64, Failure> {
        match self {
            Self::Unsigned(v) => Ok(*v),
            _ => Err("header integer".into()),
        }
    }
    pub(super) fn bytes(&self) -> Result<&[u8], Failure> {
        match self {
            Self::Bytes(v) => Ok(v),
            _ => Err("header bytes".into()),
        }
    }
}
impl Serialize for Cbor {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => s.serialize_unit(),
            Self::Bool(v) => s.serialize_bool(*v),
            Self::Unsigned(v) => s.serialize_u64(*v),
            Self::Signed(v) => s.serialize_i64(*v),
            Self::Bytes(v) => s.serialize_bytes(v),
            Self::Text(v) => s.serialize_str(v),
            Self::Array(v) => v.serialize(s),
            Self::Map(v) => {
                use ser::SerializeMap;
                let mut map = s.serialize_map(Some(v.len()))?;
                for (k, v) in v {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}
impl<'de> Deserialize<'de> for Cbor {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Cbor;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("current header CBOR")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Cbor, E> {
                Ok(Cbor::Null)
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Cbor, E> {
                Ok(Cbor::Bool(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Cbor, E> {
                Ok(Cbor::Unsigned(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Cbor, E> {
                Ok(Cbor::Signed(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Cbor, E> {
                Ok(Cbor::Text(v.into()))
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Cbor, E> {
                Ok(Cbor::Bytes(v.into()))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Cbor, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element()? {
                    values.push(v);
                }
                Ok(Cbor::Array(values))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut a: A) -> Result<Cbor, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_entry()? {
                    values.push(v);
                }
                Ok(Cbor::Map(values))
            }
        }
        d.deserialize_any(Visitor)
    }
}

pub(super) fn header(bytes: &[u8]) -> Result<(Value, Vec<Vec<u8>>), Failure> {
    let decoded: Cbor = rocky_sdk::credentials::decode_bounded(bytes, bytes.len())?;
    let fields = decoded.array()?;
    let at = |i| fields.get(i).ok_or_else(|| Failure::from("header field"));
    if at(0)?.number()? != 1 {
        return Err("unsupported header version".into());
    }
    let payload = at(3)?.number()?;
    let clock = 4 + usize::from(payload != 0);
    let sequence = at(clock + 1)?.number()?;
    let previous = clock + 2 + usize::from(sequence != 0);
    let mut dependencies = Vec::new();
    if sequence != 0 {
        dependencies.push(at(clock + 2)?.bytes()?.to_vec());
    }
    for hash in at(previous)?.array()? {
        dependencies.push(hash.bytes()?.to_vec());
    }
    Ok((
        json!({"node":at(1)?.json(),"sequence":sequence,"clock":at(clock)?.json(),
        "backlink":if sequence == 0 {Value::Null} else {at(clock+2)?.json()},
        "previous":at(previous)?.json(),"extensions":at(previous+1)?.json()}),
        dependencies,
    ))
}
