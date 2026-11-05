//! Root escrow records travel with the data they recover: before a page,
//! the session carries the records of each key epoch its frames were
//! sealed under, once per session. The peer keeps those of the frames it
//! receives, so every holder of a scope, keyless custody too, holds them.
//! A record is opaque and names nothing; it reaches only a peer that
//! receives the frames anyway.
use super::*;
use crate::wire::MAX_CARRIED_ESCROW;
use rocky_core::KeyEpoch;
use rocky_credentials::RootEscrow;
use std::collections::{BTreeMap, BTreeSet};

type Slot = (ResourceScope, KeyEpoch);

#[derive(Serialize, Deserialize)]
pub(super) enum EscrowMessage {
    /// Records of the key epochs the next page's frames name. It has no answer.
    Carry { records: Vec<(Slot, RootEscrow)> },
}

/// Lives one session, like the chain table.
#[derive(Default)]
pub(super) struct EscrowTable {
    sent: BTreeSet<Slot>,
    received: BTreeMap<Slot, Vec<RootEscrow>>,
}

fn slots(frames: &[EncryptedFrame]) -> Result<BTreeSet<Slot>, Error> {
    frames
        .iter()
        .map(|frame| Ok((frame.scope()?, frame.generation()?)))
        .collect()
}

impl NativeSession {
    /// Sends the records of the key epochs `frames` name that this session
    /// has not carried yet.
    pub(super) async fn carry_escrow<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let fresh: BTreeSet<Slot> = slots(frames)?
            .into_iter()
            .filter(|slot| self.escrow.sent.insert(*slot))
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let records = replica.escrow_of(&fresh).await?;
        if !records.is_empty() {
            self.send(EscrowMessage::Carry { records }).await?;
        }
        Ok(())
    }
    /// Keeps carried records until frames of their epoch arrive; the
    /// session's byte budget bounds them.
    pub(super) fn learn_escrow(&mut self, message: EscrowMessage) {
        let EscrowMessage::Carry { records } = message;
        for (slot, record) in records {
            let held = self.escrow.received.entry(slot).or_default();
            if held.len() < MAX_CARRIED_ESCROW && !held.contains(&record) {
                held.push(record);
            }
        }
    }
    /// Stores the carried records of the key epochs of `frames`, which
    /// this node receives.
    pub(super) async fn keep_escrow<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let mut kept = Vec::new();
        for slot in slots(frames)? {
            for record in self.escrow.received.remove(&slot).unwrap_or_default() {
                kept.push((slot, record));
            }
        }
        replica.keep_carried_escrow(kept).await
    }
}
