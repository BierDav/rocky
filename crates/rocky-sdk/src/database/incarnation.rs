use crate::{Failure, enrollment::now, secrets::Vault};
use rocky_core::TypedRows;
use rocky_secure::EncryptedReplica;
use std::path::Path;

pub(crate) const ROLE: &str = "incarnation";
const AUTHORITY_KEY: &str = "incarnation";

pub(super) struct Incarnation {
    stored: Option<u64>,
    held: Option<u64>,
}

fn count(bytes: &[u8]) -> Result<u64, Failure> {
    match bytes.split_first_chunk::<8>() {
        Some((count, padding)) if padding.len() == 24 && padding.iter().all(|byte| *byte == 0) => {
            Ok(u64::from_le_bytes(*count))
        }
        _ => Err("a database incarnation is a zero-padded 32-byte count".into()),
    }
}
fn encoded(count: u64) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&count.to_le_bytes());
    bytes
}

impl Incarnation {
    pub(super) fn read(replica: &Path, vault: &Vault) -> Result<Self, Failure> {
        let stored = rocky_secure::read_authority_entry(replica, AUTHORITY_KEY)?
            .map(|(bytes, _)| count(&bytes))
            .transpose()?;
        let held = vault
            .find(ROLE)?
            .map(|bytes| count(&bytes[..]))
            .transpose()?;
        Ok(Self { stored, held })
    }
    pub(super) fn vouched(&self) -> bool {
        self.stored.is_none() || self.stored == self.held
    }
    pub(super) async fn renew(
        self,
        replica: &EncryptedReplica<TypedRows>,
        vault: &Vault,
    ) -> Result<(), Failure> {
        let next = encoded(
            self.stored
                .max(self.held)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("the database incarnation is exhausted")?,
        );
        vault.put(ROLE, &next)?;
        replica
            .put_authority_state(AUTHORITY_KEY, &next, now()?)
            .await?;
        Ok(())
    }
}
