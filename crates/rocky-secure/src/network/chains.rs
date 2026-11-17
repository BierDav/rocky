use super::*;
use crate::wire::Chain;
use std::collections::{BTreeMap, BTreeSet};

const CARRIED_BYTES: usize = MAX_SECURE_FRAME / 2;

#[derive(Serialize, Deserialize)]
pub(super) enum ChainMessage {
    Carry { chains: Vec<Chain> },
}

#[derive(Default)]
pub(super) struct ChainTable {
    sent: BTreeSet<Commitment>,
    pub(super) received: BTreeMap<Commitment, Chain>,
    #[cfg(test)]
    pub(super) withhold: bool,
}

impl NativeSession {
    pub(super) async fn carry_chains(
        &mut self,
        frames: &mut [EncryptedFrame],
    ) -> Result<(), Error> {
        let mut chains = Vec::new();
        for frame in frames {
            frame.chain()?;
            if let Some(chain) = frame.take_chain() {
                chains.push(chain);
            }
        }
        self.carry_credentials(chains).await
    }
    pub(super) async fn carry_credentials(&mut self, chains: Vec<Chain>) -> Result<(), Error> {
        let mut batch = Vec::new();
        let mut bytes = 0;
        for chain in chains {
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
    pub(super) fn learn_chains(&mut self, message: ChainMessage) {
        let ChainMessage::Carry { chains } = message;
        for chain in chains {
            self.chains.received.insert(chain.digest(), chain);
        }
    }
    pub(super) fn complete_frames(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Result<(Vec<EncryptedFrame>, usize), Error> {
        let mut complete = Vec::with_capacity(frames.len());
        let mut incomplete = 0;
        for mut frame in frames {
            frame.take_chain();
            match self.chains.received.get(&frame.chain_digest()?) {
                Some(chain) => complete.push(frame.with_chain(chain.clone())),
                None => incomplete += 1,
            }
        }
        Ok((complete, incomplete))
    }
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
