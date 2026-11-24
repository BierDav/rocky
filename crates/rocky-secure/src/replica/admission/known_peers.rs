use super::MAX_REMEMBERED_PEERS;
use rocky_core::TipId;
use rocky_credentials::PeerId;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct KnownPeers {
    clock: u64,
    seen: BTreeMap<PeerId, Seen>,
    configured: BTreeSet<PeerId>,
}

struct Seen {
    at: u64,
    tip: Option<TipId>,
    acknowledged_at: u64,
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
        self.configured.contains(peer)
    }
    pub(super) fn last_seen(&self, peer: &PeerId) -> Option<u64> {
        self.seen.get(peer).map(|seen| seen.at)
    }
    pub(super) fn admit(&mut self, peer: PeerId, tip: TipId) {
        let at = self.tick();
        let acknowledged_at = self.seen.get(&peer).map_or(0, |seen| seen.acknowledged_at);
        self.seen.insert(
            peer,
            Seen {
                at,
                tip: Some(tip),
                acknowledged_at,
            },
        );
        self.evict_beyond_bound(Some(peer));
    }
    pub(in crate::replica) fn acknowledged(&mut self, peer: PeerId, recorded: bool) {
        let at = self.tick();
        match self.seen.get_mut(&peer) {
            Some(seen) => {
                seen.at = at;
                seen.acknowledged_at = at;
            }
            None if recorded => {
                self.seen.insert(
                    peer,
                    Seen {
                        at,
                        tip: None,
                        acknowledged_at: at,
                    },
                );
                self.evict_beyond_bound(Some(peer));
            }
            None => {}
        }
    }
    pub(super) fn configure(&mut self, peers: BTreeSet<PeerId>) {
        self.configured = peers;
        self.evict_beyond_bound(None);
    }
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
    fn evict_beyond_bound(&mut self, arriving: Option<PeerId>) {
        let unkept = |peer: &PeerId| !self.configured.contains(peer);
        let excess = self
            .seen
            .keys()
            .filter(|peer| unkept(peer))
            .count()
            .saturating_sub(MAX_REMEMBERED_PEERS);
        if excess == 0 {
            return;
        }
        let mut least_recently_acknowledged: Vec<_> = self
            .seen
            .iter()
            .filter(|(peer, _)| unkept(peer) && Some(**peer) != arriving)
            .map(|(peer, seen)| (seen.acknowledged_at, seen.at, *peer))
            .collect();
        least_recently_acknowledged.sort_unstable();
        for (_, _, peer) in least_recently_acknowledged.iter().take(excess) {
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
