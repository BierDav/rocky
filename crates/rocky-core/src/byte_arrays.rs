use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_bytes::{ByteArray, Bytes};
use std::collections::{BTreeMap, BTreeSet};

pub trait Collection<Item> {
    fn items<'a>(&'a self) -> impl Iterator<Item = &'a Item>
    where
        Item: 'a;
}
impl<Item> Collection<Item> for Vec<Item> {
    fn items<'a>(&'a self) -> impl Iterator<Item = &'a Item>
    where
        Item: 'a,
    {
        self.iter()
    }
}
impl<Item> Collection<Item> for BTreeSet<Item> {
    fn items<'a>(&'a self) -> impl Iterator<Item = &'a Item>
    where
        Item: 'a,
    {
        self.iter()
    }
}

pub mod one {
    pub use serde_bytes::{deserialize, serialize};
}

pub mod each {
    use super::*;

    pub fn serialize<C, S, const N: usize>(items: &C, serializer: S) -> Result<S::Ok, S::Error>
    where
        C: Collection<[u8; N]>,
        S: Serializer,
    {
        serializer.collect_seq(items.items().map(|item| Bytes::new(item)))
    }

    pub fn deserialize<'de, C, D, const N: usize>(deserializer: D) -> Result<C, D::Error>
    where
        C: FromIterator<[u8; N]>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<ByteArray<N>>::deserialize(deserializer)?
            .into_iter()
            .map(ByteArray::into_array)
            .collect())
    }
}

pub mod keys {
    use super::*;

    pub fn serialize<V, S, const N: usize>(
        map: &BTreeMap<[u8; N], V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        V: Serialize,
        S: Serializer,
    {
        serializer.collect_map(map.iter().map(|(key, value)| (Bytes::new(key), value)))
    }

    pub fn deserialize<'de, V, D, const N: usize>(
        deserializer: D,
    ) -> Result<BTreeMap<[u8; N], V>, D::Error>
    where
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(BTreeMap::<ByteArray<N>, V>::deserialize(deserializer)?
            .into_iter()
            .map(|(key, value)| (key.into_array(), value))
            .collect())
    }
}

pub mod tagged {
    use super::*;

    pub fn serialize<C, T, S, const N: usize>(items: &C, serializer: S) -> Result<S::Ok, S::Error>
    where
        C: Collection<(T, [u8; N])>,
        T: Serialize,
        S: Serializer,
    {
        serializer.collect_seq(items.items().map(|(tag, array)| (tag, Bytes::new(array))))
    }

    pub fn deserialize<'de, C, T, D, const N: usize>(deserializer: D) -> Result<C, D::Error>
    where
        C: FromIterator<(T, [u8; N])>,
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(T, ByteArray<N>)>::deserialize(deserializer)?
            .into_iter()
            .map(|(tag, array)| (tag, array.into_array()))
            .collect())
    }
}

pub mod pairs {
    use super::*;

    type Pairs<const N: usize, const M: usize> = Vec<([u8; N], [u8; M])>;

    pub fn serialize<S, const N: usize, const M: usize>(
        items: &[([u8; N], [u8; M])],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(
            items
                .iter()
                .map(|(first, second)| (Bytes::new(first), Bytes::new(second))),
        )
    }

    pub fn deserialize<'de, D, const N: usize, const M: usize>(
        deserializer: D,
    ) -> Result<Pairs<N, M>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(
            Vec::<(ByteArray<N>, ByteArray<M>)>::deserialize(deserializer)?
                .into_iter()
                .map(|(first, second)| (first.into_array(), second.into_array()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Arrays {
        #[serde(with = "one")]
        single: [u8; 32],
        #[serde(with = "one")]
        maybe: Option<[u8; 32]>,
        #[serde(with = "each")]
        list: Vec<[u8; 32]>,
        #[serde(with = "each")]
        set: BTreeSet<[u8; 32]>,
        #[serde(with = "keys")]
        map: BTreeMap<[u8; 32], u8>,
        #[serde(with = "tagged")]
        tagged: BTreeSet<(u8, [u8; 32])>,
        #[serde(with = "pairs")]
        pairs: Vec<([u8; 32], [u8; 32])>,
    }

    fn arrays() -> Arrays {
        Arrays {
            single: [200; 32],
            maybe: Some([201; 32]),
            list: vec![[202; 32]],
            set: BTreeSet::from([[203; 32]]),
            map: BTreeMap::from([([204; 32], 1)]),
            tagged: BTreeSet::from([(1, [205; 32])]),
            pairs: vec![([206; 32], [207; 32])],
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_fixed_size_array_encodes_as_one_cbor_byte_string_of_its_length() {
        let mut encoded = Vec::new();
        ciborium::into_writer(&arrays(), &mut encoded).unwrap();
        let byte_string_of_32 = [0x58, 32];
        assert_eq!(
            encoded
                .windows(2)
                .filter(|window| *window == byte_string_of_32)
                .count(),
            8,
            "every array is a byte string, never an integer list"
        );
        assert!(encoded.len() < 8 * 34 + 96);
        assert_eq!(
            ciborium::from_reader::<Arrays, _>(encoded.as_slice()).unwrap(),
            arrays()
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn json_keeps_arrays_as_number_lists_and_reads_them_back() {
        let unkeyed = Arrays {
            map: BTreeMap::new(),
            ..arrays()
        };
        let json = serde_json::to_string(&unkeyed).unwrap();
        assert!(json.contains(&format!("[{}]", ["200"; 32].join(","))));
        assert_eq!(serde_json::from_str::<Arrays>(&json).unwrap(), unkeyed);
    }
}
