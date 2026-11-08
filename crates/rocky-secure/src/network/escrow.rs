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
/// Credential bytes one session may copy into reconstructed vouches: each
/// carried record clones its whole referenced chain.
const RECONSTRUCTED_BYTES: usize = 8 * 1024 * 1024;

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
    /// Carried records seen, by digest: a repeat is neither stored nor charged.
    seen: BTreeSet<Commitment>,
    /// Encoded length of each referenced chain, measured once.
    chain_bytes: BTreeMap<Commitment, usize>,
    reconstructed: usize,
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
    /// session's byte budget and the reconstruction quota bound them, and
    /// none is judged first-come.
    pub(super) fn learn_escrow(&mut self, message: EscrowMessage) -> Result<(), Error> {
        let EscrowMessage::Carry { records } = message;
        for record in records {
            if !self.escrow.seen.insert(rocky_core::commitment(
                "rocky.carried-escrow.v1",
                &encode(&record)?,
            )) {
                continue;
            }
            let chain = self
                .chains
                .received
                .get(&record.chain_digest())
                .ok_or_else(|| denied("incomplete escrow chain"))?;
            let bytes = match self.escrow.chain_bytes.get(&chain.digest()) {
                Some(bytes) => *bytes,
                None => {
                    let bytes = encode(chain)?.len();
                    self.escrow.chain_bytes.insert(chain.digest(), bytes);
                    bytes
                }
            };
            self.escrow.reconstructed = self
                .escrow
                .reconstructed
                .checked_add(bytes)
                .filter(|total| *total <= RECONSTRUCTED_BYTES)
                .ok_or_else(|| denied("escrow reconstruction quota"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_credentials::RootEscrow;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn records_sharing_one_chain_are_charged_per_reconstruction() {
        let f = crate::tests::Fixture::new();
        let node = PrivateKey::new();
        let chain = crate::wire::Chain::new(f.credentials(&node, f.full())).unwrap();
        let bytes = encode(&chain).unwrap().len();
        let channels = link::channels::tests::channels();
        let mut session = channels
            .accept(link::Frame {
                channel: 2,
                through: Vec::new(),
                direction: true,
                message: link::Message::Channel(link::channels::Packet::Open { generation: 0 }),
            })
            .unwrap()
            .unwrap();
        session.learn_chains(ChainMessage::Carry {
            chains: vec![chain.clone()],
        });
        let record = RootEscrow::seal(PrivateKey::new().public_key(), [9; 32], &[7; 32]).unwrap();
        let slot = (f.public, KeyEpoch::new(1, &[7; 32]));
        let carry = |signed_at| {
            VouchedEscrow::sign(
                &node,
                chain.credentials().clone(),
                slot,
                record.clone(),
                signed_at,
            )
            .unwrap()
            .carried()
            .unwrap()
        };
        let carried = |records| EscrowMessage::Carry { records };
        // A repeated record is neither kept nor charged again.
        session
            .learn_escrow(carried(vec![carry(0), carry(0)]))
            .unwrap();
        assert_eq!(session.escrow.reconstructed, bytes);
        assert_eq!(session.escrow.received[&slot].len(), 1);
        let fit = (RECONSTRUCTED_BYTES / bytes) as u64;
        session
            .learn_escrow(carried((1..fit).map(carry).collect()))
            .unwrap();
        assert_eq!(session.escrow.reconstructed as u64, fit * bytes as u64);
        assert!(
            session
                .learn_escrow(carried(vec![carry(fit)]))
                .unwrap_err()
                .to_string()
                .contains("escrow reconstruction quota")
        );
    }
}
