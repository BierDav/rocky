//! Causal bundle units, packed independently of scope boundaries.
use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Page {
    pub frames: Vec<EncryptedFrame>,
    pub voids: Vec<SignedVoid>,
    weight: usize,
}
impl Page {
    fn fits(&self, count: usize, weight: usize) -> bool {
        self.frames.len() + self.voids.len() + count <= rows::MAX_SECURE_PAGE
            && self.weight.saturating_add(weight) <= MAX_PAGE_BYTES
    }
}
fn root(parents: &mut [usize], mut at: usize) -> usize {
    while parents[at] != at {
        parents[at] = parents[parents[at]];
        at = parents[at];
    }
    at
}

pub(super) fn pages(
    exported: Vec<RangePage>,
    members: &BTreeMap<Hash, Vec<(ResourceScope, PublicKey, u64)>>,
) -> Result<Vec<Page>, Error> {
    let mut result = Vec::<Page>::new();
    let mut frames = Vec::new();
    let mut void_ids = BTreeSet::new();
    for page in exported {
        for record in page.voids {
            if !void_ids.insert(rocky_core::commitment(
                "rocky.live-void.v1",
                &encode(&record)?,
            )) {
                continue;
            }
            let weight = record.weight();
            if result.last().is_none_or(|p| !p.fits(1, weight)) {
                result.push(Page::default());
            }
            let page = result
                .last_mut()
                .ok_or_else(|| denied("publication void page"))?;
            if !page.fits(1, weight) {
                return Err(denied("publication void quota"));
            }
            page.weight += weight;
            page.voids.push(record);
        }
        frames.extend(page.frames);
    }
    let mut positions = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for (index, frame) in frames.iter().enumerate() {
        let h = &frame.operation_ref()?.header;
        positions.insert((h.extensions.scope, h.public_key, h.seq_num), index);
        identities.insert(frame.id()?, index);
    }
    let mut parents: Vec<_> = (0..frames.len()).collect();
    for (id, required) in members {
        let Some(&index) = identities.get(id) else {
            continue;
        };
        for position in required {
            if let Some(&other) = positions.get(position) {
                let a = root(&mut parents, index);
                let b = root(&mut parents, other);
                parents[a.max(b)] = a.min(b);
            }
        }
    }
    let mut units = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..frames.len() {
        let group = root(&mut parents, index);
        units.entry(group).or_default().push(index);
    }
    let mut dependencies = BTreeMap::<usize, BTreeSet<usize>>::new();
    for (index, frame) in frames.iter().enumerate() {
        let h = &frame.operation_ref()?.header;
        let group = parents[index];
        for prior in h.previous.iter().chain(h.backlink.iter()) {
            if let Some(&previous) = identities.get(prior) {
                let before = parents[previous];
                if before != group {
                    dependencies.entry(group).or_default().insert(before);
                }
            }
        }
    }
    let mut pending: BTreeSet<_> = units.keys().copied().collect();
    let mut frames: Vec<_> = frames.into_iter().map(Some).collect();
    // Voids precede data; do not combine their intake with a following unit.
    let mut append = false;
    while !pending.is_empty() {
        let group = pending
            .iter()
            .copied()
            .find(|group| {
                dependencies
                    .get(group)
                    .is_none_or(|deps| deps.iter().all(|id| !pending.contains(id)))
            })
            .ok_or_else(|| denied("publication dependency cycle"))?;
        pending.remove(&group);
        let unit = units
            .get(&group)
            .ok_or_else(|| denied("publication unit"))?;
        let weight = unit.iter().try_fold(0usize, |sum, index| {
            let frame = frames[*index]
                .as_ref()
                .ok_or_else(|| denied("publication duplicate unit"))?;
            Ok::<_, Error>(sum.saturating_add(frame.weight()))
        })?;
        let whole = unit.len() <= rows::MAX_SECURE_PAGE && weight <= MAX_PAGE_BYTES;
        if !append || !whole || result.last().is_none_or(|p| !p.fits(unit.len(), weight)) {
            result.push(Page::default());
        }
        for index in unit {
            let frame = frames[*index]
                .take()
                .ok_or_else(|| denied("publication duplicate unit"))?;
            let weight = frame.weight();
            if result.last().is_none_or(|p| !p.fits(1, weight)) {
                result.push(Page::default());
            }
            let page = result
                .last_mut()
                .ok_or_else(|| denied("publication data page"))?;
            if !page.fits(1, weight) {
                return Err(denied("publication item quota"));
            }
            page.weight += weight;
            page.frames.push(frame);
        }
        append = true;
    }
    Ok(result)
}
