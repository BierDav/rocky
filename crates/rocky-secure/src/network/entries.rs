//! Coordinated entries between trusted nodes: a proposer stores an assigned
//! entry on every in-sync node, which acknowledges it to the coordinator
//! itself; a node catching up reads the committed entries' log positions and
//! frame ids, whose frames arrive by ordinary range sync once committed. See
//! `docs/runtime/coordinated-writes.md`.
use super::*;
use crate::replica::{CommittedEntry, CoordinatedEntry, MAX_CATCH_UP_ENTRIES};
use rocky_core::{LogPending, TipLog};
use rocky_credentials::SignedCarry;

/// Bytes of frames and voids in one store page; a larger frame goes alone.
const PAGE_BYTES: usize = MAX_SECURE_FRAME / 2;

#[derive(Serialize, Deserialize)]
pub(super) enum EntryMessage {
    /// One page of an assigned entry; the last one stores it.
    Store {
        log: TipLog,
        pending: LogPending,
        frames: Vec<EncryptedFrame>,
        voids: Vec<SignedVoid>,
        /// The same on every page of an entry.
        carry: Option<Box<SignedCarry>>,
        last: bool,
    },
    Committed {
        log: TipLog,
        after: u64,
    },
    /// Committed entries without frames.
    Entries {
        entries: Vec<CommittedEntry>,
    },
}
/// An entry a peer is sending page by page.
pub(super) type Receiving = (
    TipLog,
    LogPending,
    Vec<EncryptedFrame>,
    Vec<SignedVoid>,
    Option<Box<SignedCarry>>,
);

impl NativeSession {
    /// Stores `entry` on the peer, which answers once it stored it durably
    /// and acknowledged it to the coordinator.
    pub async fn store_entry(&mut self, entry: &CoordinatedEntry) -> Result<(), Error> {
        let mut pages: Vec<(Vec<EncryptedFrame>, Vec<SignedVoid>)> = vec![Default::default()];
        let mut used = 0;
        let weights = entry
            .voids
            .iter()
            .map(|void| (None, Some(void), void.weight()))
            .chain(
                entry
                    .frames
                    .iter()
                    .map(|frame| (Some(frame), None, frame.weight())),
            );
        for (frame, void, weight) in weights {
            let page = pages.last_mut().ok_or_else(|| denied("entry page"))?;
            if used + weight > PAGE_BYTES && !(page.0.is_empty() && page.1.is_empty()) {
                pages.push(Default::default());
                used = 0;
            }
            used += weight;
            let page = pages.last_mut().ok_or_else(|| denied("entry page"))?;
            if let Some(frame) = frame {
                page.0.push(frame.clone());
            }
            if let Some(void) = void {
                page.1.push(void.clone());
            }
        }
        let count = pages.len();
        for (index, (mut frames, voids)) in pages.into_iter().enumerate() {
            self.carry_chains(&mut frames).await?;
            self.send(EntryMessage::Store {
                log: entry.log,
                pending: entry.pending.clone(),
                frames,
                voids,
                carry: entry.carry.clone().map(Box::new),
                last: index + 1 == count,
            })
            .await?;
            self.acknowledged("coordinated entry not stored").await?;
        }
        Ok(())
    }
    /// The committed entries of `log` after `after` the peer holds, without
    /// frames.
    pub async fn committed_entries(
        &mut self,
        log: TipLog,
        after: u64,
    ) -> Result<Vec<CommittedEntry>, Error> {
        self.send(EntryMessage::Committed { log, after }).await?;
        let EntryMessage::Entries { entries } = self.reply().await? else {
            return Err(denied("unexpected committed entries"));
        };
        if entries.len() > MAX_CATCH_UP_ENTRIES
            || entries
                .iter()
                .any(|entry| !entry.frames.is_empty() || entry.ids.len() > MAX_BUNDLE_FRAMES)
        {
            return Err(denied("committed entries outside the request"));
        }
        Ok(entries)
    }
    pub(super) async fn answer_entries<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: EntryMessage,
    ) -> Result<(), Error> {
        match message {
            EntryMessage::Store {
                log,
                pending,
                frames,
                voids,
                carry,
                last,
            } => {
                if pending.proposer != *self.peer.as_bytes() {
                    return Err(denied("only its proposer sends an entry"));
                }
                let (frames, incomplete) = self.complete_frames(frames)?;
                if incomplete > 0 {
                    return Err(denied(crate::wire::INCOMPLETE_FRAME));
                }
                let receiving = self.entry.get_or_insert_with(|| {
                    (log, pending.clone(), Vec::new(), Vec::new(), carry.clone())
                });
                if receiving.0 != log
                    || receiving.1 != pending
                    || receiving.4 != carry
                    || receiving.2.len() + frames.len() > MAX_BUNDLE_FRAMES
                    || receiving.3.len() + voids.len() > MAX_BUNDLE_FRAMES
                {
                    self.entry = None;
                    return Err(denied("entry pages outside one entry"));
                }
                receiving.2.extend(frames);
                receiving.3.extend(voids);
                if last {
                    let (log, pending, frames, voids, carry) =
                        self.entry.take().ok_or_else(|| denied("entry page"))?;
                    let coordinator = replica
                        .domain_log()
                        .ok_or_else(|| denied("this node takes part in no coordinated log"))?;
                    replica
                        .store_entry(
                            CoordinatedEntry {
                                log,
                                pending,
                                frames,
                                voids,
                                carry: carry.map(|carry| *carry),
                            },
                            coordinator.as_ref(),
                        )
                        .await
                        .map_err(|error| denied(&error.to_string()))?;
                }
                self.send(Message::Acknowledged).await
            }
            EntryMessage::Committed { log, after } => {
                let mut entries = replica
                    .committed_entries(self.peer, &self.credentials, log, after)
                    .await?;
                // Committed frames travel by range sync.
                for entry in &mut entries {
                    entry.frames.clear();
                    entry.voids.clear();
                }
                self.send(EntryMessage::Entries { entries }).await
            }
            EntryMessage::Entries { .. } => Err(denied("unexpected entries request")),
        }
    }
}
