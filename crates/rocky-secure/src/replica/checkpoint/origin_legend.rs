//! Page origins share manifests and binding digests without changing their commitments.
use super::*;
use serde_bytes::{ByteArray, ByteBuf};

type Digest = ByteArray<32>;
type Origin = (Digest, Digest, Digest);
#[derive(Serialize, Deserialize)]
struct Legend(Vec<Digest>, Vec<Digest>, Vec<(u32, u32, Digest)>);

pub(super) fn serialize<S: serde::Serializer>(
    origins: &[ByteBuf],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let origins: Vec<Origin> = origins
        .iter()
        .map(|bytes| decode(bytes))
        .collect::<Result<_, _>>()
        .map_err(serde::ser::Error::custom)?;
    let manifests: Vec<_> = origins
        .iter()
        .map(|origin| origin.0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let bindings: Vec<_> = origins
        .iter()
        .map(|origin| origin.1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let entries = origins
        .into_iter()
        .map(|(manifest, binding, header)| {
            let manifest = manifests
                .binary_search(&manifest)
                .map_err(serde::ser::Error::custom)?;
            let binding = bindings
                .binary_search(&binding)
                .map_err(serde::ser::Error::custom)?;
            Ok((
                u32::try_from(manifest).map_err(serde::ser::Error::custom)?,
                u32::try_from(binding).map_err(serde::ser::Error::custom)?,
                header,
            ))
        })
        .collect::<Result<_, S::Error>>()?;
    Legend(manifests, bindings, entries).serialize(serializer)
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ByteBuf>, D::Error> {
    use serde::de::Error as _;
    let Legend(manifests, bindings, entries) = Legend::deserialize(deserializer)?;
    if manifests.windows(2).any(|pair| pair[0] >= pair[1])
        || bindings.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(D::Error::custom(
            "checkpoint origin dictionaries are not strictly ordered",
        ));
    }
    let mut used_manifests = BTreeSet::new();
    let mut used_bindings = BTreeSet::new();
    let origins = entries
        .into_iter()
        .map(|(manifest, binding, header)| {
            let source = manifests
                .get(manifest as usize)
                .ok_or_else(|| D::Error::custom("checkpoint origin lacks a manifest"))?;
            let chain = bindings
                .get(binding as usize)
                .ok_or_else(|| D::Error::custom("checkpoint origin lacks bindings"))?;
            used_manifests.insert(manifest);
            used_bindings.insert(binding);
            encode(&(*source, *chain, header))
                .map(ByteBuf::from)
                .map_err(D::Error::custom)
        })
        .collect::<Result<_, _>>()?;
    if used_manifests.len() != manifests.len() || used_bindings.len() != bindings.len() {
        return Err(D::Error::custom(
            "checkpoint origin carries unused dictionary entries",
        ));
    }
    Ok(origins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize)]
    struct Page(#[serde(with = "super")] Vec<ByteBuf>);

    #[test]
    fn origin_dictionaries_preserve_witnesses_and_refuse_ambiguous_references() {
        let origins: Vec<_> = (0..32)
            .map(|index| {
                ByteBuf::from(
                    encode(&(
                        Digest::new([1; 32]),
                        Digest::new([2; 32]),
                        Digest::new([index; 32]),
                    ))
                    .unwrap(),
                )
            })
            .collect();
        let bytes = encode(&Page(origins.clone())).unwrap();
        assert!(bytes.len() < encode(&origins).unwrap().len() / 2);
        assert_eq!(decode::<Page>(&bytes).unwrap().0, origins);
        for defect in 0..5 {
            let mut legend = Legend(
                vec![Digest::new([1; 32])],
                vec![Digest::new([2; 32])],
                vec![(0, 0, Digest::new([3; 32]))],
            );
            match defect {
                0 => legend.0.clear(),
                1 => legend.1.clear(),
                2 => legend.0.push(legend.0[0]),
                3 => legend.1.insert(0, Digest::new([4; 32])),
                _ => legend.0.push(Digest::new([5; 32])),
            }
            // A tuple wrapper matches Page's newtype representation.
            assert!(decode::<Page>(&encode(&legend).unwrap()).is_err());
        }
    }
}
