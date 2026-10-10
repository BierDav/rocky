use super::*;
use crate::storage::rows;
use p2panda_core::Signature;
use rocky_core::{
    Commitment, Coordinator, ResourceScope, Right, RowCommandApplication, StrongHead,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct EffectJob {
    /// Provider idempotency key, unchanged across executor takeover and delivery attempts.
    pub id: Commitment,
    pub scope: ResourceScope,
    payload: Vec<u8>,
    result: ConfirmedCommand,
    index: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectState {
    Pending,
    Uncertain,
    Failed,
    Succeeded,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum EffectOutcome {
    Succeeded(Vec<u8>),
    Failed(Vec<u8>),
    Uncertain(Vec<u8>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Event {
    Started { time: u64, inputs: Vec<u8> },
    Finished { time: u64, outcome: EffectOutcome },
}
/// Authenticated external inputs/outcomes, encrypted to the job's reader scope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectObservation {
    job: Commitment,
    attempt: Commitment,
    started: bool,
    pub(super) result: ConfirmedCommand,
    index: usize,
    head: StrongHead,
    node: PublicKey,
    credentials: CredentialChain,
    event: EncryptedPackage,
    signature: Signature,
}
#[derive(Clone, Debug)]
pub struct EffectAttempt {
    pub job: EffectJob,
    pub id: Commitment,
}
impl EffectJob {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
impl EffectObservation {
    pub(crate) fn settled<A: rocky_core::RowApplication>(
        &self,
        inner: &super::replica::Inner<A>,
    ) -> Result<bool, Error> {
        let Some(key) = inner.opening_key(self.scope(), self.event.generation) else {
            return Ok(false);
        };
        Ok(!self.started
            && matches!(
                decode::<Event>(&self.event.open(&key.key)?)?,
                Event::Finished {
                    outcome: EffectOutcome::Succeeded(_) | EffectOutcome::Failed(_),
                    ..
                }
            ))
    }

    pub fn scope(&self) -> ResourceScope {
        self.event.scope
    }
    fn signing_bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.effect-observation.v1",
            &(
                self.job,
                self.attempt,
                self.started,
                (
                    self.result.strong_package()?.encode()?,
                    self.result.receipt()?.encode(),
                ),
                self.index,
                &self.head,
                self.node,
                &self.credentials,
                &self.event,
            ),
        )
    }
    fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.effect-record.v1",
            &encode(self)?,
        ))
    }
    fn verify<A: RowCommandApplication>(
        &self,
        inner: &super::replica::Inner<A>,
        historical: bool,
    ) -> Result<(), Error> {
        let package = self.result.verify_history(inner)?;
        let part = self
            .result
            .parts
            .iter()
            .find(|part| {
                part.commitment().ok()
                    == PackageSet::from_bytes(&package.rows)
                        .ok()
                        .and_then(|set| set.commitments.get(self.index).copied())
            })
            .ok_or_else(|| denied("job output missing"))?;
        let none = RevocationSet::new();
        let revocations = if historical {
            &none
        } else {
            &inner.config.revocations
        };
        let time = if historical {
            self.credentials
                .0
                .last()
                .ok_or_else(|| denied("missing effect credential"))?
                .capability
                .not_before
        } else {
            now()?
        };
        let grants = inner.verify_chain_with(&self.credentials, self.node, revocations, time)?;
        if self.job != job_id(&package.request, self.index)?
            || part.kind != PackageKind::Job
            || part.scope != self.event.scope
            || self.head.scope != part.scope.domain
            || self.head.executor != secure_node_id(self.node)
            || self.head.epoch == 0
            || self.head.sequence < package.sequence
            || self.event.kind != PackageKind::Job
            || !grants.iter().any(|grant| {
                grant.scope == part.scope
                    && grant.rows == rocky_core::RowSelection::All
                    && grant.rights.contains(&Right::Execute)
                    && grant.rights.contains(&Right::Read)
            })
            || !self.node.verify(&self.signing_bytes()?, &self.signature)
        {
            return Err(denied("effect observation identity/authority denied"));
        }
        Ok(())
    }
}
fn job_id(request: &rocky_core::StrongRequest, index: usize) -> Result<Commitment, Error> {
    Ok(rocky_core::commitment(
        "rocky.effect-job.v1",
        &encode(&(request.hash()?, index as u64))?,
    ))
}
impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Discover jobs exclusively in locally retained, confirmed results.
    pub async fn effect_jobs(&self, scope: ResourceScope) -> Result<Vec<EffectJob>, Error> {
        let inner = self.inner.lock().await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("job read denied"));
        }
        if !inner.config.keys.iter().any(|key| key.scope == scope) {
            return Err(denied("job key unavailable"));
        }
        let mut jobs = Vec::new();
        for record in rows(
            &inner.conn,
            "SELECT result FROM secure_results ORDER BY domain,sequence",
        )
        .await?
        {
            let [rocky_core::SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("corrupt effect source"));
            };
            let result: ConfirmedCommand = decode(bytes)?;
            let package = result.verify_history(&inner)?;
            let binding = PackageSet::from_bytes(&package.rows)?;
            for part in &result.parts {
                if part.scope != scope || part.kind != PackageKind::Job {
                    continue;
                }
                let opening = inner
                    .opening_key(scope, part.generation)
                    .ok_or_else(|| denied("job key generation unavailable"))?;
                let index = binding
                    .commitments
                    .iter()
                    .position(|id| part.commitment().ok().as_ref() == Some(id))
                    .ok_or_else(|| denied("unbound job"))?;
                jobs.push(EffectJob {
                    id: job_id(&package.request, index)?,
                    scope,
                    payload: part.open(&opening.key)?,
                    result: result.clone(),
                    index,
                });
            }
        }
        Ok(jobs)
    }
    pub async fn effect_state(&self, job: &EffectJob) -> Result<EffectState, Error> {
        let inner = self.inner.lock().await;
        effect_state(&inner, job).await
    }
    /// Persist send intent before calling a provider. Uncertainty requires an explicit retry choice.
    pub async fn begin_effect(
        &self,
        job: EffectJob,
        recorded_inputs: Vec<u8>,
        retry_uncertain: bool,
        coordinator: &dyn Coordinator,
    ) -> Result<EffectAttempt, Error> {
        coordinator.verify(&job.result.receipt()?).await?;
        let head = coordinator.head(job.scope.domain).await?;
        let mut inner = self.inner.lock().await;
        if head.executor != secure_node_id(inner.config.node.public_key()) || head.epoch == 0 {
            return Err(denied("effect executor is fenced"));
        }
        if head.schema != inner.definition.schema || head.handler != inner.definition.handler {
            return Err(denied("effect handler generation unavailable"));
        }
        let applied = crate::storage::collect_rows(
            inner
                .conn
                .query(
                    "SELECT sequence,head FROM secure_command_rows WHERE scope=?",
                    [encode(&job.scope)?],
                )
                .await?,
        )
        .await?;
        if applied.first()
            != Some(&vec![
                rocky_core::SqlValue::Integer(head.sequence as i64),
                rocky_core::SqlValue::Blob(head.head.to_vec()),
            ])
        {
            return Err(denied("effect executor missing confirmed state"));
        }
        let attempt = transaction!(inner, |inner| {
            match effect_state(inner, &job).await? {
                EffectState::Succeeded => return Err(denied("effect already succeeded")),
                EffectState::Uncertain if !retry_uncertain => {
                    return Err(denied("effect delivery uncertain; explicit retry required"));
                }
                _ => {}
            }
            let count = observations(inner, job.id).await?.len();
            let id = rocky_core::commitment(
                "rocky.effect-attempt.v1",
                &encode(&(
                    job.id,
                    inner.config.node.public_key(),
                    head.epoch,
                    count as u64,
                ))?,
            );
            let observation = make_observation(
                inner,
                &job,
                id,
                head.clone(),
                true,
                Event::Started {
                    time: now()?,
                    inputs: recorded_inputs.clone(),
                },
            )?;
            observation.verify(inner, false)?;
            persist(inner, &observation).await?;
            Ok(EffectAttempt {
                job: job.clone(),
                id,
            })
        })?;
        // The provider is called next; its start record must survive.
        inner.flush()?;
        Ok(attempt)
    }
    /// Recording may fail after a successful send. The retained start then remains uncertain.
    pub async fn finish_effect(
        &self,
        attempt: &EffectAttempt,
        outcome: EffectOutcome,
        coordinator: &dyn Coordinator,
    ) -> Result<(), Error> {
        let receipt = attempt.job.result.receipt()?;
        coordinator.verify(&receipt).await?;
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            let records = observations(inner, attempt.job.id).await?;
            let start = records
                .iter()
                .find(|record| {
                    record.attempt == attempt.id
                        && record.started
                        && record.node == inner.config.node.public_key()
                })
                .ok_or_else(|| denied("unknown local effect attempt"))?;
            let observation = make_observation(
                inner,
                &attempt.job,
                attempt.id,
                start.head.clone(),
                false,
                Event::Finished {
                    time: now()?,
                    outcome: outcome.clone(),
                },
            )?;
            observation.verify(inner, false)?;
            if let Some(old) = records
                .iter()
                .find(|record| record.attempt == attempt.id && !record.started)
            {
                let key = |event: &EncryptedPackage| {
                    inner
                        .opening_key(event.scope, event.generation)
                        .ok_or_else(|| denied("effect key unavailable"))
                };
                let old_event: Event = decode(&old.event.open(&key(&old.event)?.key)?)?;
                let new_event: Event =
                    decode(&observation.event.open(&key(&observation.event)?.key)?)?;
                if let (
                    Event::Finished { outcome: old, .. },
                    Event::Finished { outcome: new, .. },
                ) = (old_event, new_event)
                {
                    return if old == new {
                        Ok(())
                    } else {
                        Err(Error::Conflict)
                    };
                }
                return Err(Error::Conflict);
            }
            #[cfg(test)]
            if inner.fail_before_commit > 0 {
                inner.fail_before_commit -= 1;
                return Err(Error::InjectedFailure);
            }
            persist(inner, &observation).await
        })?;
        // Delivery records are evidence of an external side effect.
        inner.flush()
    }
    /// The provider future runs after the start commits, with no runtime or coordinator locks held.
    pub async fn dispatch_effect<F, Fut>(
        &self,
        job: EffectJob,
        inputs: Vec<u8>,
        retry_uncertain: bool,
        coordinator: &dyn Coordinator,
        provider: F,
    ) -> Result<EffectOutcome, Error>
    where
        F: FnOnce(EffectJob) -> Fut,
        Fut: std::future::Future<Output = EffectOutcome>,
    {
        let attempt = self
            .begin_effect(job, inputs, retry_uncertain, coordinator)
            .await?;
        let outcome = provider(attempt.job.clone()).await;
        self.finish_effect(&attempt, outcome.clone(), coordinator)
            .await?;
        Ok(outcome)
    }
    pub async fn receive_effect_observation(
        &self,
        observation: EffectObservation,
        coordinator: &dyn Coordinator,
    ) -> Result<(), Error> {
        coordinator.verify(&observation.result.receipt()?).await?;
        let mut inner = self.inner.lock().await;
        observation.verify(&inner, true)?;
        super::recovery::authorize(&inner.local_grants(now()?)?, &[observation.scope()])?;
        transaction!(inner, |inner| persist(inner, &observation).await)
    }
}
fn make_observation<A: RowCommandApplication>(
    inner: &super::replica::Inner<A>,
    job: &EffectJob,
    attempt: Commitment,
    head: StrongHead,
    started: bool,
    event: Event,
) -> Result<EffectObservation, Error> {
    let key = inner
        .config
        .keys
        .iter()
        .find(|key| key.scope == job.scope)
        .ok_or_else(|| denied("effect key unavailable"))?;
    let mut record = EffectObservation {
        job: job.id,
        attempt,
        started,
        result: job.result.clone(),
        index: job.index,
        head,
        node: inner.config.node.public_key(),
        credentials: inner.config.credentials.clone(),
        event: EncryptedPackage::seal(
            job.scope,
            key.generation,
            PackageKind::Job,
            &key.key,
            &encode(&event)?,
        )?,
        signature: inner.config.node.sign(b""),
    };
    record.signature = inner.config.node.sign(&record.signing_bytes()?);
    Ok(record)
}
async fn persist<A: RowCommandApplication>(
    inner: &super::replica::Inner<A>,
    observation: &EffectObservation,
) -> Result<(), Error> {
    let bytes = encode(observation)?;
    if bytes.len() > MAX_SECURE_FRAME {
        return Err(denied("effect observation quota"));
    }
    let totals = rows(
        &inner.conn,
        "SELECT COUNT(*),COALESCE(SUM(LENGTH(observation)),0) FROM secure_effect_observations",
    )
    .await?;
    let Some(
        [
            rocky_core::SqlValue::Integer(count),
            rocky_core::SqlValue::Integer(size),
        ],
    ) = totals.first().map(Vec::as_slice)
    else {
        return Err(denied("corrupt effect budget"));
    };
    if *count >= MAX_SECURE_RECORDS as i64 || *size as usize + bytes.len() > MAX_SECURE_BYTES {
        return Err(denied("effect storage quota"));
    }
    inner
        .conn
        .execute(
            "INSERT INTO secure_effect_observations VALUES(?,?,?) ON CONFLICT(id) DO NOTHING",
            [observation.id()?.to_vec(), observation.job.to_vec(), bytes],
        )
        .await?;
    Ok(())
}
async fn observations<A: RowCommandApplication>(
    inner: &super::replica::Inner<A>,
    job: Commitment,
) -> Result<Vec<EffectObservation>, Error> {
    let records = crate::storage::collect_rows(
        inner
            .conn
            .query(
                "SELECT observation FROM secure_effect_observations WHERE job=? ORDER BY id",
                [job.to_vec()],
            )
            .await?,
    )
    .await?;
    records
        .into_iter()
        .map(|row| {
            let [rocky_core::SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(denied("corrupt observation"));
            };
            decode(bytes)
        })
        .collect()
}
async fn effect_state<A: RowCommandApplication>(
    inner: &super::replica::Inner<A>,
    job: &EffectJob,
) -> Result<EffectState, Error> {
    if !inner.can_read_key(job.scope, &inner.local_grants(now()?)?) {
        return Err(denied("effect state read denied"));
    }
    if !inner.config.keys.iter().any(|key| key.scope == job.scope) {
        return Err(denied("effect key unavailable"));
    }
    let mut attempts = BTreeMap::new();
    for record in observations(inner, job.id).await? {
        record.verify(inner, true)?;
        if record.scope() != job.scope {
            return Err(denied("effect observation scope mismatch"));
        }
        let opening = inner
            .opening_key(job.scope, record.event.generation)
            .ok_or_else(|| denied("effect observation key generation unavailable"))?;
        match decode::<Event>(&record.event.open(&opening.key)?)? {
            Event::Started { .. } if record.started => {
                attempts
                    .entry(record.attempt)
                    .or_insert(EffectState::Uncertain);
            }
            Event::Finished { outcome, .. } if !record.started => {
                let state = match outcome {
                    EffectOutcome::Succeeded(_) => EffectState::Succeeded,
                    EffectOutcome::Failed(_) => EffectState::Failed,
                    EffectOutcome::Uncertain(_) => EffectState::Uncertain,
                };
                if attempts
                    .get(&record.attempt)
                    .is_some_and(|old| *old != EffectState::Uncertain && *old != state)
                {
                    return Err(denied("conflicting effect outcomes"));
                }
                attempts.insert(record.attempt, state);
            }
            _ => return Err(denied("effect event kind mismatch")),
        }
    }
    if attempts
        .values()
        .any(|state| *state == EffectState::Succeeded)
    {
        Ok(EffectState::Succeeded)
    } else if attempts
        .values()
        .any(|state| *state == EffectState::Uncertain)
    {
        Ok(EffectState::Uncertain)
    } else if attempts.is_empty() {
        Ok(EffectState::Pending)
    } else {
        Ok(EffectState::Failed)
    }
}
