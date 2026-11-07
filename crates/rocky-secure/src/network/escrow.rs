//! Root escrow records travel with the data they recover: before a page,
//! the session carries the records of each key epoch its frames were
//! sealed under, once per session, as the key holders that sealed them
//! vouched for them. The peer keeps those of the frames it receives that a
//! key holder vouched for, so every holder of a scope, keyless custody
//! too, holds them, and a forged record never displaces a genuine one.
//! A record is opaque and names nothing; it reaches only a peer that
//! receives the frames anyway.
use super::*;
use rocky_core::KeyEpoch;
use rocky_credentials::{CarriedEscrow, VouchedEscrow};
use std::collections::{BTreeMap, BTreeSet};

type Slot = (ResourceScope, KeyEpoch);

#[derive(Serialize, Deserialize)]
pub(super) enum EscrowMessage {
    /// Records of the key epochs the next page's frames name. It has no answer.
    Carry { records: Vec<CarriedEscrow> },
}

/// Lives one session, like the chain table.
#[derive(Default)]
pub(super) struct EscrowTable {
    sent: BTreeSet<Slot>,
    received: BTreeMap<Slot, Vec<VouchedEscrow>>,
    /// Records sent ahead of the genuine ones, as a forging peer would.
    #[cfg(test)]
    forged: Vec<VouchedEscrow>,
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
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut records = replica.escrow_of(&fresh).await?;
        #[cfg(test)]
        records.splice(0..0, std::mem::take(&mut self.escrow.forged));
        if !records.is_empty() {
            self.carry_credentials(
                records
                    .iter()
                    .map(|record| crate::wire::Chain::new(record.chain().clone()))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .await?;
            let records = records
                .iter()
                .map(VouchedEscrow::carried)
                .collect::<Result<Vec<_>, _>>()
                .map_err(credential)?;
            self.send(EscrowMessage::Carry { records }).await?;
        }
        Ok(())
    }
    /// Holds carried records until frames of their epoch arrive; the
    /// session's byte budget bounds them, and none is judged first-come.
    pub(super) fn learn_escrow(&mut self, message: EscrowMessage) -> Result<(), Error> {
        let EscrowMessage::Carry { records } = message;
        for record in records {
            let chain = self
                .chains
                .received
                .get(&record.chain_digest())
                .ok_or_else(|| denied("incomplete escrow chain"))?;
            let record = record
                .complete(chain.credentials().clone())
                .map_err(credential)?;
            self.escrow
                .received
                .entry(record.slot())
                .or_default()
                .push(record);
        }
        Ok(())
    }
    /// Judges and stores the carried records of the key epochs of
    /// `frames`, which this node receives.
    pub(super) async fn keep_escrow<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let mut kept = Vec::new();
        for slot in slots(frames)? {
            kept.extend(self.escrow.received.remove(&slot).unwrap_or_default());
        }
        replica.keep_carried_escrow(self.peer, kept).await
    }
    #[cfg(test)]
    pub(crate) fn forge_escrow(&mut self, records: Vec<VouchedEscrow>) {
        self.escrow.forged = records;
    }
}
