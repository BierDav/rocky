//! Proposing and submitting owner-signed cuts from a trusted node; see
//! decision 0074 and `docs/runtime/stuck-recovery.md`.
use super::*;
use crate::replica::checkpoint::CoordinatedBinding;
use cut::{Base, classify};
use rocky_credentials::{CutPoint, CutStatement, SignedCut};

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // A signed cut flushed before the coordinator sees it, until decided.
    "CREATE TABLE IF NOT EXISTS secure_cut_submissions(log BLOB PRIMARY KEY,digest BLOB NOT NULL,cut BLOB NOT NULL) WITHOUT ROWID",
];

/// A cut a trusted node proposes for the domain owner to sign.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CutProposal {
    pub statement: CutStatement,
    /// Every scope of the log this node projects.
    pub bases: Vec<BaseReport>,
}
/// Whether a scope's replay base survives the proposed cut. A scope that
/// is not clean fails closed with `cut_base_missing` after the cut until
/// a clean checkpoint is imported.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaseReport {
    pub scope: ResourceScope,
    /// The binding of the scope's verified checkpoint; none without one.
    pub binding: Option<CoordinatedBinding>,
    pub clean: bool,
}
impl CutProposal {
    /// Signs the proposal with the domain owner's key. `authority` is the
    /// owner's issuer chain; empty when the owner is the publisher.
    pub fn sign(
        &self,
        owner: &PrivateKey,
        authority: CredentialChain,
    ) -> Result<CutEvidence, Error> {
        Ok(CutEvidence {
            record: SignedCut::sign(self.statement.clone(), owner).map_err(credential)?,
            authority,
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Collects every committed entry any reachable device holds, then
    /// proposes a cut of the exact head that retains this node's position.
    /// Nothing is stored. Fails with `Behind` until this node holds every
    /// earlier cut and the prefix the previous one retained.
    pub async fn cut_proposal(
        &self,
        log: Scope,
        coordinator: &dyn DomainLog,
        peers: &dyn EntryPeers,
    ) -> Result<CutProposal, CoordinatedError> {
        match self.resolve_coordinated(log, coordinator, peers).await {
            Ok(_) | Err(CoordinatedError::Behind) => {}
            Err(error) => return Err(error),
        }
        let head = coordinator.log_head(log).await.map_err(coordination)?;
        // A pending entry may still commit; the coordinator decides it first.
        if head.pending.is_some() {
            return Err(CoordinatedError::Conflict);
        }
        let (sequence, previous) = match &head.cut {
            Some(cut) => {
                let record = CutEvidence::from_log(cut)?.record;
                let digest = record.digest().map_err(credential)?;
                (record.statement.sequence + 1, Some(digest))
            }
            None => (1, None),
        };
        let inner = self.inner.lock().await;
        let held = inner.held_cuts(log).to_vec();
        let (position, running, _, _) = inner.log_state(log).await?;
        let running = running.ok_or(CoordinatedError::Behind)?;
        if held.len() as u64 + 1 != sequence
            || held.last().map(|cut| cut.digest) != previous
            || held
                .last()
                .is_some_and(|cut| position < cut.statement.entry())
        {
            return Err(CoordinatedError::Behind);
        }
        if position > head.commit {
            return Err(Error::Corrupt(
                "this node's coordinated log is ahead of the coordinator".into(),
            )
            .into());
        }
        // The window authenticates this node's position; before it, only the
        // genesis anchor does and the owner endorses the rest.
        let first = head.commit + 1 - head.hashes.len() as u64;
        let retained = CutPoint { position, running };
        let verified = if position + 1 >= first {
            let mut chained = running;
            for at in position + 1..=head.commit {
                let hash = head.committed(at).ok_or(CoordinatedError::Behind)?;
                chained = coordinated_chain(chained, at, hash);
            }
            if chained != head.running_hash {
                return Err(Error::Corrupt(
                    "this node's coordinated log forks the coordinator's window".into(),
                )
                .into());
            }
            retained
        } else {
            CutPoint {
                position: 0,
                running: head.genesis,
            }
        };
        let statement = CutStatement {
            log,
            sequence,
            previous,
            verified,
            retained,
            replaced: CutPoint {
                position: head.commit,
                running: head.running_hash,
            },
            epoch: head.epoch,
            issued: now()?,
        };
        let mut bases = Vec::new();
        for scope in inner.log_scopes(log).await? {
            let Some(id) = inner.projected(scope).await? else {
                continue;
            };
            let binding = inner
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
                .map(|signed| signed.statement.coordinated);
            let clean = !inner.cut_guarded(scope).await?
                && binding.is_none_or(|binding| {
                    classify(&binding, &held) == Base::Clean && binding.highest <= position
                });
            bases.push(BaseReport {
                scope,
                binding,
                clean,
            });
        }
        Ok(CutProposal { statement, bases })
    }
    /// Submits an owner-signed cut. It is flushed before the coordinator
    /// sees it, sent again identically after a lost reply, and admitted
    /// here once the coordinator commits it; a refused record is discarded.
    /// Returns the position the cut occupies.
    pub async fn submit_cut(
        &self,
        cut: &CutEvidence,
        coordinator: &dyn DomainLog,
    ) -> Result<u64, CoordinatedError> {
        let log = cut.record.statement.log;
        cut.to_log()?;
        let digest = cut.record.digest().map_err(credential)?;
        {
            let mut inner = self.inner.lock().await;
            inner.verify_cut(cut).await?;
            let bytes = encode(cut)?;
            transaction!(inner, |inner| {
                inner
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO secure_cut_submissions VALUES(?,?,?)",
                        [encode(&log)?, digest.to_vec(), bytes.clone()],
                    )
                    .await?;
                Ok(())
            })?;
            inner.flush()?;
        }
        let _deciding = self.domain_log.deciding.lock().await;
        self.send_cut(cut, coordinator).await
    }
    /// Sends a flushed submission and decides it.
    async fn send_cut(
        &self,
        cut: &CutEvidence,
        coordinator: &dyn DomainLog,
    ) -> Result<u64, CoordinatedError> {
        let log = cut.record.statement.log;
        let entry = cut.record.statement.entry();
        let digest = cut.record.digest().map_err(credential)?;
        match coordinator.cut(log, cut.to_log()?).await {
            Ok(LogResult::Committed { position, .. }) if position == entry => {}
            Ok(_) => {
                return Err(Error::Corrupt("the coordinator placed a cut elsewhere".into()).into());
            }
            Err(error) => {
                let refused = coordination(error);
                if refused == CoordinatedError::Unavailable {
                    return Err(refused);
                }
                // A lost reply may have admitted it before the head moved on.
                let head = coordinator.log_head(log).await.map_err(coordination)?;
                if head.committed(entry) != Some(digest) {
                    self.discard_cut(log, digest).await?;
                    return Err(refused);
                }
            }
        }
        self.receive_cuts(std::slice::from_ref(cut)).await?;
        self.discard_cut(log, digest).await?;
        Ok(entry)
    }
    async fn discard_cut(&self, log: Scope, digest: Commitment) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_cut_submissions WHERE log=? AND digest=?",
                    [encode(&log)?, digest.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        inner.flush()
    }
    async fn cut_submission(&self, log: Scope) -> Result<Option<CutEvidence>, Error> {
        let inner = self.inner.lock().await;
        let record = collect_rows(
            inner
                .conn
                .query(
                    "SELECT cut FROM secure_cut_submissions WHERE log=?",
                    [encode(&log)?],
                )
                .await?,
        )
        .await?;
        match record.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => Ok(Some(decode(bytes)?)),
            Some(_) => Err(Error::Corrupt("cut submission".into())),
            None => Ok(None),
        }
    }
    /// Sends a submission a lost reply or restart left undecided again; a
    /// refused one is discarded and resolution goes on.
    pub(super) async fn retry_cut(
        &self,
        log: Scope,
        coordinator: &dyn DomainLog,
    ) -> Result<(), CoordinatedError> {
        let Some(cut) = self.cut_submission(log).await? else {
            return Ok(());
        };
        match self.send_cut(&cut, coordinator).await {
            Ok(_)
            | Err(
                CoordinatedError::Unavailable
                | CoordinatedError::Conflict
                | CoordinatedError::Refused(_),
            ) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
