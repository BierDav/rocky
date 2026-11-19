use super::MAX_UNCONFIGURED_PEERS;
use rocky_core::{ResourceScope, TipId};
use rocky_credentials::PeerId;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct KnownPeers {
    clock: u64,
    seen: BTreeMap<PeerId, Seen>,
    configured: BTreeSet<PeerId>,
    custody: BTreeMap<ResourceScope, BTreeSet<PeerId>>,
    kept: BTreeSet<PeerId>,
}

struct Seen {
    at: u64,
    tip: Option<TipId>,
}

impl KnownPeers {
    pub(in crate::replica) fn tip(&self, peer: &PeerId) -> Option<TipId> {
        self.seen.get(peer).and_then(|seen| seen.tip)
    }
    pub(in crate::replica) fn admitted(&self) -> impl Iterator<Item = &PeerId> {
        self.seen
            .iter()
            .filter(|(_, seen)| seen.tip.is_some())
            .map(|(peer, _)| peer)
    }
    pub(super) fn kept(&self, peer: &PeerId) -> bool {
        self.kept.contains(peer)
    }
    pub(super) fn last_seen(&self, peer: &PeerId) -> Option<u64> {
        self.seen.get(peer).map(|seen| seen.at)
    }
    pub(super) fn admit(&mut self, peer: PeerId, tip: TipId) {
        let at = self.tick();
        self.seen.insert(peer, Seen { at, tip: Some(tip) });
        self.evict_beyond_bound();
    }
    pub(in crate::replica) fn acknowledged(&mut self, peer: PeerId, recorded: bool) {
        let at = self.tick();
        match self.seen.get_mut(&peer) {
            Some(seen) => seen.at = at,
            None if recorded => {
                self.seen.insert(peer, Seen { at, tip: None });
                self.evict_beyond_bound();
            }
            None => {}
        }
    }
    pub(super) fn configure(&mut self, peers: BTreeSet<PeerId>) {
        self.configured = peers;
        self.rekeep();
    }
    pub(in crate::replica) fn set_custody(
        &mut self,
        scope: ResourceScope,
        peers: BTreeSet<PeerId>,
    ) {
        if peers.is_empty() {
            self.custody.remove(&scope);
        } else {
            self.custody.insert(scope, peers);
        }
        self.rekeep();
    }
    fn rekeep(&mut self) {
        self.kept = self
            .custody
            .values()
            .flatten()
            .chain(&self.configured)
            .copied()
            .collect();
        self.evict_beyond_bound();
    }
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
    fn evict_beyond_bound(&mut self) {
        let evictable = |(peer, _): &(&PeerId, &Seen)| !self.kept.contains(*peer);
        let excess = self
            .seen
            .iter()
            .filter(evictable)
            .count()
            .saturating_sub(MAX_UNCONFIGURED_PEERS);
        if excess == 0 {
            return;
        }
        let mut oldest: Vec<_> = self
            .seen
            .iter()
            .filter(evictable)
            .map(|(peer, seen)| (seen.at, *peer))
            .collect();
        oldest.sort_unstable();
        for (_, peer) in &oldest[..excess] {
            self.seen.remove(peer);
        }
    }
    #[cfg(test)]
    pub(crate) fn remembered(&self) -> usize {
        self.seen.len()
    }
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.seen.clear();
    }
}
