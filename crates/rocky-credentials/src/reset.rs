//! Owner resets: "sign out everywhere else". A root statement names the
//! only nodes that stay and fences the data of every other node by the
//! signer's applied log positions, including nodes it never saw.
//!
//! Without a shared sequence, "certified before the reset" is causal: a
//! reset ends every node it does not keep unless a reset that follows it
//! (references it, transitively) keeps that node. Concurrent resets do not
//! follow each other, so each ends what the other does not keep. Ending is
//! decided by the held statements alone, never by arrival order.
use crate::{Error, PeerId, PublicKey, denied};
use rocky_core::{Commitment, ResourceScope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Nodes one reset may keep: the signer and its named custodians.
pub const MAX_RESET_KEEP: usize = 64;
/// Earlier resets one reset may follow.
pub const MAX_RESET_FOLLOWS: usize = 64;
/// Fenced writer logs one reset may record.
pub const MAX_FENCE_LOGS: usize = 2048;
/// Largest encoded signed reset statement; it carries its fence.
pub const MAX_RESET_BYTES: usize = 192 * 1024;

/// The keep-list and data fence of one owner reset.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reset {
    /// The resets the signer held unfollowed when signing: this reset
    /// follows them and every reset they follow.
    pub follows: BTreeSet<Commitment>,
    /// The only nodes it leaves valid: its signer and named custodians.
    pub keep: BTreeSet<PeerId>,
    /// The signer's applied prefix of every log of an ended writer, ordered
    /// by scope without repeats. A log it does not list keeps nothing.
    pub fence: Vec<FenceScope>,
}
/// The fenced writer logs of one scope, ordered by writer without repeats.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceScope {
    pub scope: ResourceScope,
    pub logs: Vec<FenceLog>,
}
/// One writer's log in a scope: positions below `next` stay valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FenceLog {
    /// The writing node's public key.
    pub writer: PublicKey,
    /// The first log position the signer had not applied; never zero.
    pub next: u64,
}
impl Reset {
    /// Canonical and bounded; fences only writers it ends.
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
    /// Whether the fence keeps `position` of `writer`'s log in `scope`.
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

/// How the held resets judge one operation of a writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetStanding {
    /// No reset ends the writer.
    Live,
    /// Resets end the writer, and every one of their fences keeps the
    /// operation: it stands as written before the reset.
    Covered,
    /// A reset ends the writer and its fence does not keep the operation.
    Fenced,
}

/// Every held reset, with what follows what. Derived from the set.
#[derive(Clone, Debug, Default)]
pub(crate) struct Resets {
    resets: BTreeMap<Commitment, Reset>,
    /// Each held reset's transitive predecessors, held or not.
    ancestors: BTreeMap<Commitment, BTreeSet<Commitment>>,
}
impl Resets {
    pub(crate) fn new(resets: BTreeMap<Commitment, Reset>) -> Self {
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
        Self { resets, ancestors }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.resets.is_empty()
    }
    /// Whether reset `id` ends `node`: it does not keep the node, and no
    /// reset that follows it does.
    fn applies(&self, id: &Commitment, reset: &Reset, node: &PeerId) -> bool {
        !reset.keep.contains(node)
            && !self.ancestors.iter().any(|(later, ancestors)| {
                ancestors.contains(id) && self.resets[later].keep.contains(node)
            })
    }
    /// The resets that end `node`, in ID order.
    pub(crate) fn ending(&self, node: &PeerId) -> impl Iterator<Item = (&Commitment, &Reset)> {
        self.resets
            .iter()
            .filter(move |(id, reset)| self.applies(id, reset, node))
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
    /// Held resets no held reset follows: what a new reset follows.
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
    /// Nodes every head keeps: the nodes no held reset ends among those
    /// the resets name.
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
