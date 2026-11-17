use crate::{DatabaseContext, Error, PeerId, PrivateKey, PublicKey, Signature, denied};
use rocky_core::{Commitment, ResourceScope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_RESET_KEEP: usize = 64;
pub const MAX_RESET_FOLLOWS: usize = 64;
pub const MAX_FENCE_LOGS: usize = 2048;
pub const MAX_RESET_BYTES: usize = 192 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reset {
    pub follows: BTreeSet<Commitment>,
    pub keep: BTreeSet<PeerId>,
    pub fence: Vec<FenceScope>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceScope {
    pub scope: ResourceScope,
    pub logs: Vec<FenceLog>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceLog {
    pub writer: PublicKey,
    pub next: u64,
}
impl Reset {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let logs: usize = self.fence.iter().map(|scope| scope.logs.len()).sum();
        if self.keep.is_empty()
            || self.keep.len() > MAX_RESET_KEEP
            || self.follows.len() > MAX_RESET_FOLLOWS
            || logs > MAX_FENCE_LOGS
            || !self
                .fence
                .windows(2)
                .all(|pair| pair[0].scope < pair[1].scope)
            || self.fence.iter().any(|scope| {
                scope.logs.is_empty()
                    || !scope
                        .logs
                        .windows(2)
                        .all(|pair| pair[0].writer < pair[1].writer)
                    || scope
                        .logs
                        .iter()
                        .any(|log| log.next == 0 || self.keep.contains(&PeerId::of(log.writer)))
            })
        {
            return Err(denied("invalid reset"));
        }
        Ok(())
    }
    pub fn covers(&self, scope: &ResourceScope, writer: &PublicKey, position: u64) -> bool {
        self.fence
            .binary_search_by(|fenced| fenced.scope.cmp(scope))
            .ok()
            .and_then(|index| {
                let logs = &self.fence[index].logs;
                logs.binary_search_by(|log| log.writer.cmp(writer))
                    .ok()
                    .map(|log| position < logs[log].next)
            })
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetAdmission {
    pub database: DatabaseContext,
    pub node: PeerId,
    pub follows: BTreeSet<Commitment>,
    pub signer: PublicKey,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedResetAdmission {
    pub admission: ResetAdmission,
    signature: Signature,
}
const ADMISSION: &str = "rocky.reset.admission.v1";
impl SignedResetAdmission {
    pub fn sign(
        key: &PrivateKey,
        database: DatabaseContext,
        node: PeerId,
        follows: BTreeSet<Commitment>,
    ) -> Result<Self, Error> {
        let admission = ResetAdmission {
            database,
            node,
            follows,
            signer: key.public_key(),
        };
        let signature = key.sign(&crate::signing_bytes(ADMISSION, &admission)?);
        let signed = Self {
            admission,
            signature,
        };
        signed.verify()?;
        Ok(signed)
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.reset.admission.id.v1",
            &crate::encode(self)?,
        ))
    }
    pub(crate) fn verify(&self) -> Result<(), Error> {
        let admission = &self.admission;
        if admission.follows.is_empty()
            || admission.follows.len() > MAX_RESET_FOLLOWS
            || !admission.signer.verify(
                &crate::signing_bytes(ADMISSION, admission)?,
                &self.signature,
            )
        {
            return Err(denied("invalid admission"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetStanding {
    Live,
    Covered,
    Fenced,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Resets {
    resets: BTreeMap<Commitment, Reset>,
    ancestors: BTreeMap<Commitment, BTreeSet<Commitment>>,
    admitted: BTreeMap<PeerId, BTreeSet<Commitment>>,
}
impl Resets {
    pub(crate) fn new(
        resets: BTreeMap<Commitment, Reset>,
        admissions: impl IntoIterator<Item = (PeerId, BTreeSet<Commitment>, PeerId)>,
    ) -> Self {
        let mut ancestors = BTreeMap::new();
        for id in resets.keys() {
            let mut seen = BTreeSet::new();
            let mut next: Vec<Commitment> = resets[id].follows.iter().copied().collect();
            while let Some(previous) = next.pop() {
                if seen.insert(previous)
                    && let Some(reset) = resets.get(&previous)
                {
                    next.extend(reset.follows.iter().copied());
                }
            }
            ancestors.insert(*id, seen);
        }
        let mut held = Self {
            resets,
            ancestors,
            admitted: BTreeMap::new(),
        };
        let admissions: Vec<(PeerId, BTreeSet<Commitment>, PeerId)> = admissions
            .into_iter()
            .map(|(node, follows, signer)| {
                let after = follows
                    .iter()
                    .filter_map(|id| Some(std::iter::once(id).chain(held.ancestors.get(id)?)))
                    .flatten()
                    .filter(|id| held.resets.contains_key(*id))
                    .copied()
                    .collect();
                (node, after, signer)
            })
            .collect();
        loop {
            let mut counted = Vec::new();
            for (node, after, signer) in &admissions {
                for id in after {
                    if !held.admitted.get(node).is_some_and(|set| set.contains(id))
                        && !held.applies(id, signer)
                    {
                        counted.push((*node, *id));
                    }
                }
            }
            if counted.is_empty() {
                return held;
            }
            for (node, id) in counted {
                held.admitted.entry(node).or_default().insert(id);
            }
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.resets.is_empty()
    }
    fn applies(&self, id: &Commitment, node: &PeerId) -> bool {
        self.resets
            .get(id)
            .is_some_and(|reset| !reset.keep.contains(node))
            && !self.ancestors.iter().any(|(later, ancestors)| {
                ancestors.contains(id)
                    && self
                        .resets
                        .get(later)
                        .is_some_and(|reset| reset.keep.contains(node))
            })
            && !self
                .admitted
                .get(node)
                .is_some_and(|after| after.contains(id))
    }
    pub(crate) fn ending(&self, node: &PeerId) -> impl Iterator<Item = (&Commitment, &Reset)> {
        self.resets
            .iter()
            .filter(move |(id, _)| self.applies(id, node))
    }
    pub(crate) fn ends(&self, node: &PeerId) -> bool {
        self.ending(node).next().is_some()
    }
    pub(crate) fn standing(
        &self,
        scope: &ResourceScope,
        writer: &PublicKey,
        position: u64,
    ) -> ResetStanding {
        let node = PeerId::of(*writer);
        let mut ending = self.ending(&node).peekable();
        if ending.peek().is_none() {
            return ResetStanding::Live;
        }
        if ending.all(|(_, reset)| reset.covers(scope, writer, position)) {
            ResetStanding::Covered
        } else {
            ResetStanding::Fenced
        }
    }
    pub(crate) fn heads(&self) -> BTreeSet<Commitment> {
        self.resets
            .keys()
            .filter(|id| {
                !self
                    .ancestors
                    .values()
                    .any(|ancestors| ancestors.contains(*id))
            })
            .copied()
            .collect()
    }
    pub(crate) fn kept(&self) -> BTreeSet<PeerId> {
        self.resets
            .values()
            .flat_map(|reset| reset.keep.iter().copied())
            .filter(|node| !self.ends(node))
            .collect()
    }
}

#[cfg(test)]
mod tests;
