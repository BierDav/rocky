//! Session-bound partition proofs; decision 0052 defines the wire contract.
use super::*;
use chacha20poly1305::{AeadInPlace, ChaCha20Poly1305, KeyInit};
use std::collections::BTreeSet;

const HOLDING_DENIED: &str = "ciphertext custody denied";
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct HoldingProof {
    pub epoch: rocky_core::KeyEpoch,
    pub value: [u8; 16],
}
#[derive(Serialize, Deserialize)]
pub(super) enum HoldingMessage {
    Prove {
        scopes: Vec<ResourceScope>,
        proofs: Vec<(ResourceScope, Vec<HoldingProof>)>,
        partitions: Vec<(ResourceScope, rocky_core::PartitionTuple)>,
    },
    Proved {
        scopes: Vec<ResourceScope>,
    },
}
fn proof_cipher(key: &KeyMaterial, binding: Commitment) -> Result<ChaCha20Poly1305, Error> {
    let bytes = encode(&(key.key, key.scope, key.generation, binding))?;
    let derived = rocky_core::commitment("rocky.holding-proof-key.v1", &bytes);
    Ok(ChaCha20Poly1305::new((&derived).into()))
}
pub(crate) fn key_proof(key: &KeyMaterial, binding: Commitment) -> Result<[u8; 16], Error> {
    proof_cipher(key, binding)?
        .encrypt_in_place_detached((&[0; 12]).into(), b"rocky.holding-proof.v1", &mut [])
        .map(Into::into)
        .map_err(|_| denied(HOLDING_DENIED))
}
pub(crate) fn verify_key_proof(
    key: &KeyMaterial,
    binding: Commitment,
    value: &[u8; 16],
) -> Result<bool, Error> {
    Ok(proof_cipher(key, binding)?
        .decrypt_in_place_detached(
            (&[0; 12]).into(),
            b"rocky.holding-proof.v1",
            &mut [],
            value.into(),
        )
        .is_ok())
}

impl NativeSession {
    pub(super) async fn accept_proofs<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
        proofs: &[(ResourceScope, Vec<HoldingProof>)],
        partitions: &[(ResourceScope, rocky_core::PartitionTuple)],
    ) -> Result<(), Error> {
        let distinct: BTreeSet<_> = scopes.iter().copied().collect();
        let valid = |members: Vec<ResourceScope>| {
            members.len() <= scopes.len()
                && members.iter().all(|scope| distinct.contains(scope))
                && members.iter().copied().collect::<BTreeSet<_>>().len() == members.len()
        };
        if scopes.is_empty()
            || scopes.len() > 64
            || distinct.len() != scopes.len()
            || !valid(proofs.iter().map(|(scope, _)| *scope).collect())
            || !valid(partitions.iter().map(|(scope, _)| *scope).collect())
            || proofs.iter().any(|(_, values)| values.len() > 4)
        {
            return Err(denied(HOLDING_DENIED));
        }
        let mut accepted = Vec::new();
        for scope in scopes {
            let values = proofs
                .iter()
                .find(|(s, _)| s == scope)
                .map_or(&[][..], |(_, p)| p.as_slice());
            let tuple = partitions.iter().find(|(s, _)| s == scope).map(|(_, t)| t);
            if replica
                .verify_holding(
                    self.peer,
                    &self.credentials,
                    self.binding,
                    *scope,
                    values,
                    tuple,
                )
                .await?
            {
                accepted.push(*scope);
            }
        }
        // Registration is only for tuples whose proof and grants verified.
        let offers: Vec<_> = partitions
            .iter()
            .filter(|(scope, _)| accepted.contains(scope))
            .cloned()
            .collect();
        replica
            .accept_custody_offers(self.peer, &self.credentials, scopes, &offers)
            .await?;
        self.proved.extend(accepted);
        Ok(())
    }

    pub(super) async fn require_holding<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        if replica.holding_required(scope).await && !self.proved.contains(&scope) {
            return Err(denied(HOLDING_DENIED));
        }
        Ok(())
    }

    /// Proves named partitions before scope enumeration or a custom transfer.
    /// Ordinary scoped client calls and frontier batches do this automatically.
    pub async fn prove_scopes<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        self.ensure_holding(replica, scopes).await
    }

    pub(super) async fn ensure_holding<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        let mut missing = Vec::new();
        for scope in scopes {
            if !self.offered.contains(scope) && replica.holding_required(*scope).await {
                missing.push(*scope);
            }
        }
        for batch in missing.chunks(64) {
            // Retain the existing rule against disclosing metadata to an ungranted server.
            let allowed = replica
                .peer_holds(self.peer, &self.credentials, batch)
                .await?;
            if allowed.len() != batch.len() {
                return Err(denied(HOLDING_DENIED));
            }
            let proofs = replica.holding_proofs(self.binding, batch).await?;
            let partitions = replica
                .tuple_proofs(self.peer, &self.credentials, batch)
                .await?;
            self.send(HoldingMessage::Prove {
                scopes: batch.to_vec(),
                proofs,
                partitions,
            })
            .await?;
            let HoldingMessage::Proved { scopes: accepted } =
                self.reply::<HoldingMessage>().await?
            else {
                return Err(denied(HOLDING_DENIED));
            };
            if accepted != batch {
                return Err(denied(HOLDING_DENIED));
            }
            self.offered.extend(accepted);
        }
        Ok(())
    }

    pub(super) async fn answer_holdings<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: HoldingMessage,
    ) -> Result<(), Error> {
        let HoldingMessage::Prove {
            scopes,
            proofs,
            partitions,
        } = message
        else {
            return Err(denied(HOLDING_DENIED));
        };
        self.accept_proofs(replica, &scopes, &proofs, &partitions)
            .await?;
        let mut accepted = Vec::new();
        for scope in scopes {
            if self.require_holding(replica, scope).await.is_ok()
                && replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await
                    .is_ok()
            {
                accepted.push(scope);
            }
        }
        self.send(HoldingMessage::Proved { scopes: accepted }).await
    }

    pub(super) async fn guard_message<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: &Message,
    ) -> Result<(), Error> {
        let scopes = match message {
            Message::Rows(message) => match message {
                RowMessage::Pull { scopes, .. } => scopes.clone(),
                RowMessage::Ack { id, .. } => vec![
                    *self
                        .served_frames
                        .get(id)
                        .ok_or_else(|| denied(HOLDING_DENIED))?,
                ],
                RowMessage::Range { frontier, .. } | RowMessage::Held { frontier } => {
                    vec![frontier.scope]
                }
                RowMessage::FrontierQuery { scope }
                | RowMessage::Push { scope, .. }
                | RowMessage::Read { scope, .. } => vec![*scope],
                _ => Vec::new(),
            },
            Message::Checkpoints(
                CheckpointMessage::CheckpointOffer { scope }
                | CheckpointMessage::CheckpointPull { scope, .. },
            ) => vec![*scope],
            Message::Horizon(
                HorizonMessage::MergeableCut { scope }
                | HorizonMessage::HorizonRejoined { scope, .. }
                | HorizonMessage::HorizonAck { scope, .. }
                | HorizonMessage::Horizon { scope, .. },
            ) => vec![*scope],
            Message::Existence(message) => match message {
                ExistenceMessage::BaseOffer { scope, .. }
                | ExistenceMessage::BaseStage { scope, .. }
                | ExistenceMessage::BaseFinish { scope }
                | ExistenceMessage::BasePull { scope, .. }
                | ExistenceMessage::Frontier { scope }
                | ExistenceMessage::Pull { scope, .. }
                | ExistenceMessage::Push { scope, .. } => vec![*scope],
                ExistenceMessage::BaseBegin { attachment } => vec![attachment.scope],
                _ => Vec::new(),
            },
            Message::Commands(message) => {
                return self.require_command_holding(replica, message).await;
            }
            _ => Vec::new(),
        };
        for scope in scopes {
            self.require_holding(replica, scope).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod primitives {
    use super::*;
    #[test]
    fn proof_binds_key_scope_epoch_and_session() {
        let scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            table: [4; 32],
            encryption: [5; 32],
        };
        let key = KeyMaterial::new(scope, 1, [6; 32]);
        let proof = key_proof(&key, [7; 32]).unwrap();
        assert!(verify_key_proof(&key, [7; 32], &proof).unwrap());
        assert!(!verify_key_proof(&key, [8; 32], &proof).unwrap());
        assert!(!verify_key_proof(&KeyMaterial::new(scope, 2, [6; 32]), [7; 32], &proof).unwrap());
        assert!(!verify_key_proof(&KeyMaterial::new(scope, 1, [9; 32]), [7; 32], &proof).unwrap());
        let other = ResourceScope {
            encryption: [10; 32],
            ..scope
        };
        assert!(
            !verify_key_proof(
                &KeyMaterial::at(other, key.generation, key.key),
                [7; 32],
                &proof
            )
            .unwrap()
        );
    }
}
