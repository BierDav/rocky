//! The session's chain table. A frame header names its author's credential
//! chain by digest; the session carries each distinct chain once, in a
//! message of its own before the first page whose frames name it, and no
//! later page repeats it. The table shares bytes, never authority: every
//! frame's chain is verified as the frame is received. See
//! `docs/decisions/0053-chains-by-reference.md`.
use super::*;
use crate::wire::Chain;
use std::collections::{BTreeMap, BTreeSet};

/// Encoded chain bytes in one message; a single larger chain goes alone.
const CARRIED_BYTES: usize = MAX_SECURE_FRAME / 2;

#[derive(Serialize, Deserialize)]
pub(super) enum ChainMessage {
    /// Chains the frames of the next page name. It has no answer.
    Carry { chains: Vec<Chain> },
}

/// Lives one session: nothing of it is stored or offered to another.
#[derive(Default)]
pub(super) struct ChainTable {
    sent: BTreeSet<Commitment>,
    received: BTreeMap<Commitment, Chain>,
    /// Marks chains sent without sending them, as a faulty peer would.
    #[cfg(test)]
    pub(super) withhold: bool,
}

impl NativeSession {
    /// Sends the chains of `frames` this session has not carried yet and
    /// leaves the frames without them.
    pub(super) async fn carry_chains(
        &mut self,
        frames: &mut [EncryptedFrame],
    ) -> Result<(), Error> {
        let mut batch = Vec::new();
        let mut bytes = 0;
        for frame in frames {
            frame.chain()?;
            let Some(chain) = frame.take_chain() else {
                continue;
            };
            if !self.chains.sent.insert(chain.digest()) {
                continue;
            }
            #[cfg(test)]
            if self.chains.withhold {
                continue;
            }
            let size = encode(&chain)?.len();
            if !batch.is_empty() && bytes + size > CARRIED_BYTES {
                self.send(ChainMessage::Carry {
                    chains: std::mem::take(&mut batch),
                })
                .await?;
                bytes = 0;
            }
            bytes += size;
            batch.push(chain);
        }
        if !batch.is_empty() {
            self.send(ChainMessage::Carry { chains: batch }).await?;
        }
        Ok(())
    }
    /// Keeps the chains the peer carried; the session's byte budget bounds them.
    pub(super) fn learn_chains(&mut self, message: ChainMessage) {
        let ChainMessage::Carry { chains } = message;
        for chain in chains {
            self.chains.received.insert(chain.digest(), chain);
        }
    }
    /// The frames whose chain this session carried, with it, and the number
    /// of the others. Those are incomplete: they are neither received nor
    /// acknowledged, so a later pass on another session sends them again.
    pub(super) fn complete_frames(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Result<(Vec<EncryptedFrame>, usize), Error> {
        let mut complete = Vec::with_capacity(frames.len());
        let mut incomplete = 0;
        for mut frame in frames {
            // Only the table supplies a chain: one sent inside a page is ignored.
            frame.take_chain();
            match self.chains.received.get(&frame.chain_digest()?) {
                Some(chain) => complete.push(frame.with_chain(chain.clone())),
                None => incomplete += 1,
            }
        }
        Ok((complete, incomplete))
    }
    /// Encoded bytes of the chains the peer carried on this session.
    pub fn carried_chain_bytes(&self) -> usize {
        self.chains
            .received
            .values()
            .map(|chain| encode(chain).map_or(0, |bytes| bytes.len()))
            .sum()
    }
    #[cfg(test)]
    pub(crate) fn withhold_chains(&mut self) {
        self.chains.withhold = true;
    }
}
