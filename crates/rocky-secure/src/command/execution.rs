use super::ticket::{CANCELLED, OUTCOME_UNKNOWN, PREPARED};
use super::*;
use crate::PendingReason;
use rocky_core::{CommandCall, CommandFailure, CoordinationError, RowCommandOutput};

/// What a handler decides on: read under the replica lock at one confirmed
/// predecessor, and read again before its output is prepared.
#[derive(PartialEq)]
struct CommandInput {
    request: StrongRequest,
    scopes: Vec<ResourceScope>,
    state: CommandRows,
    arguments: Vec<u8>,
}
fn preparation_key(request: &StrongRequest, base: &StrongHead) -> Result<Commitment, Error> {
    Ok(rocky_core::commitment(
        "rocky.encrypted-preparation.v1",
        &encode(&(
            request.hash()?,
            base.configuration,
            base.executor.0,
            base.epoch,
            base.sequence,
            base.head,
        ))?,
    ))
}
impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Coordinator I/O occurs between durable preparation and local result transactions.
    pub async fn prepare_encrypted(
        &self,
        command: EncryptedCommand,
        coordinator: &dyn Coordinator,
    ) -> Result<ConfirmedCommand, Error> {
        let _executing = self.executing.enter().await?;
        self.prepare(command, coordinator).await
    }
    async fn prepare(
        &self,
        command: EncryptedCommand,
        coordinator: &dyn Coordinator,
    ) -> Result<ConfirmedCommand, Error> {
        let request = {
            let inner = self.inner.lock().await;
            let request = command.verify(&inner)?;
            let (state, sequence) = inner.progress_of(&request.hash()?).await?;
            if state == super::ticket::CONFIRMED
                && let Some(sequence) = sequence
            {
                let mut retained = inner
                    .conn
                    .query(
                        "SELECT 1 FROM secure_results WHERE domain=? AND sequence=?",
                        bindings(vec![
                            SqlValue::Blob(encode(&request.scope)?),
                            SqlValue::Integer(sequence as i64),
                        ]),
                    )
                    .await?;
                if retained.next().await?.is_none() {
                    return Err(Error::Pruned {
                        node: request.id.node,
                        sequence: request.id.sequence.get(),
                    });
                }
            }
            request
        };
        let accepted = coordinator.lookup(request.scope, request.id).await?;
        if accepted.is_none() {
            let scopes = {
                let inner = self.inner.lock().await;
                resources(&inner, &request)?
            };
            for scope in scopes {
                if !self.command_snapshot(scope).await?.complete {
                    // Retryable: a sync from a custodian completes it.
                    return Err(Error::StrongPending(PendingReason::MissingConfirmedPackage));
                }
            }
        }
        let mut result = if let Some(receipt) = &accepted {
            let inner = self.inner.lock().await;
            let retained = rows(&inner.conn, "SELECT result FROM secure_preparations UNION ALL SELECT result FROM secure_results").await?;
            let mut found = None;
            for record in retained {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("corrupt preparation"));
                };
                let candidate: ConfirmedCommand = decode(bytes)?;
                if candidate.strong_package()?.confirmation()? == receipt.confirmation {
                    found = Some(candidate);
                    break;
                }
            }
            found.ok_or_else(|| {
                denied("confirmed encrypted payload must be recovered from its executor")
            })?
        } else {
            let base = coordinator.head(request.scope).await?;
            let key = preparation_key(&request, &base)?;
            let (retained, input, app) = {
                let inner = self.inner.lock().await;
                if base.executor != secure_node_id(inner.config.node.public_key())
                    || base.epoch == 0
                {
                    return Err(denied("encrypted executor is fenced"));
                }
                // The handler of this generation runs only once the
                // coordinator activated it; a bare local upgrade waits.
                if (base.schema, base.handler)
                    != (inner.definition.schema, inner.definition.handler)
                {
                    return Err(Error::Coordination(
                        rocky_core::CoordinationError::Generations,
                    ));
                }
                let mut retained = inner
                    .conn
                    .query(
                        "SELECT result FROM secure_preparations WHERE request=?",
                        [key.to_vec()],
                    )
                    .await?;
                match retained.next().await? {
                    Some(row) => (Some(row.get::<Vec<u8>>(0)?), None, inner.app.clone()),
                    None => {
                        let input = inner.command_input(&command, &base).await?;
                        (None, Some(input), inner.app.clone())
                    }
                }
            };
            match (retained, input) {
                (Some(bytes), _) => decode(&bytes)?,
                (None, Some(input)) => {
                    // The handler runs without the replica lock or a
                    // transaction, on the state read at `base`.
                    let output = app
                        .run_command(CommandCall {
                            method: &request.method,
                            version: request.version,
                            request: request.id,
                            caller: request.caller,
                            arguments: &input.arguments,
                            state: &input.state,
                            predecessor: (base.sequence, base.head),
                        })
                        .await
                        .map_err(|failure| match failure {
                            CommandFailure::Invalid(message) => denied(&message),
                            CommandFailure::Retryable(message) => Error::Handler(message),
                        })?;
                    // The head or the fence may have moved meanwhile.
                    if coordinator.head(request.scope).await? != base {
                        return Err(Error::StrongPending(PendingReason::StaleFence));
                    }
                    let mut inner = self.inner.lock().await;
                    transaction!(inner, |inner| {
                        let result = inner
                            .prepare_command(&command, base.clone(), &input, output.clone())
                            .await?;
                        let bytes = result.to_bytes()?;
                        inner.command_budget(bytes.len()).await?;
                        inner
                            .conn
                            .execute(
                                "INSERT INTO secure_preparations VALUES(?,?)",
                                [key.to_vec(), bytes],
                            )
                            .await?;
                        inner.index_preparation(&key, &result).await?;
                        Ok(result)
                    })?
                }
                (None, None) => return Err(denied("missing command input")),
            }
        };
        {
            let mut inner = self.inner.lock().await;
            result.verify_preparation(&inner)?;
            if result.strong_package()?.request != request {
                return Err(denied("retained preparation request mismatch"));
            }
            let hash = request.hash()?;
            transaction!(inner, |inner| inner.progress(&hash, PREPARED, None).await)?;
            // The preparation is confirmed with the coordinator next.
            inner.flush()?;
        }
        if let Some(receipt) = accepted {
            result.receipt = receipt.encode();
        }
        Ok(result)
    }
    /// Prepares and confirms one command at a time on this replica.
    pub async fn execute_encrypted(
        &self,
        command: EncryptedCommand,
        coordinator: &dyn Coordinator,
    ) -> Result<ConfirmedCommand, Error> {
        let _executing = self.executing.enter().await?;
        let result = self.prepare(command, coordinator).await?;
        self.confirm_encrypted(
            result,
            coordinator,
            &DurabilityPolicy::AsyncReplication,
            &[],
        )
        .await
    }
    /// Required custody must be collected after preparation and before this confirmation call.
    pub async fn confirm_encrypted(
        &self,
        mut result: ConfirmedCommand,
        coordinator: &dyn Coordinator,
        policy: &DurabilityPolicy,
        acknowledgments: &[CustodyAck],
    ) -> Result<ConfirmedCommand, Error> {
        {
            let inner = self.inner.lock().await;
            result.verify_preparation(&inner)?;
        }
        let package = result.strong_package()?;
        let complete = PackageSet::from_bytes(&package.rows)?;
        let retained: BTreeSet<_> = result
            .parts
            .iter()
            .map(EncryptedPackage::commitment)
            .collect::<Result<_, _>>()?;
        if complete.commitments.iter().any(|id| !retained.contains(id)) {
            return Err(denied(
                "confirmation requires full prepared payload closure",
            ));
        }
        let receipt = if let Some(receipt) = coordinator
            .lookup(package.request.scope, package.request.id)
            .await?
        {
            if receipt.confirmation != package.confirmation()? {
                return Err(denied("confirmation differs from preparation"));
            }
            receipt
        } else {
            self.recovery_durability(package.request.scope)
                .await?
                .verify(self, &result, acknowledgments)
                .await?;
            policy.verify(self, &result, acknowledgments).await?;
            let hash = package.request.hash()?;
            // From here a lost reply leaves the outcome unknown.
            self.mark(&hash, OUTCOME_UNKNOWN).await?;
            match coordinator.confirm(package.confirmation()?).await {
                Ok(receipt) => receipt,
                Err(error) => {
                    // An explicit refusal establishes that nothing was accepted.
                    if !matches!(
                        error,
                        CoordinationError::Unavailable | CoordinationError::AlreadyConfirmed(_)
                    ) {
                        let mut inner = self.inner.lock().await;
                        transaction!(inner, |inner| inner.unsubmitted(&hash).await)?;
                    }
                    return Err(error.into());
                }
            }
        };
        result.receipt = receipt.encode();
        self.receive_encrypted_result(result.clone(), coordinator)
            .await?;
        // The applied result is returned for delivery to peers.
        self.flush().await?;
        Ok(result)
    }
    /// Durably records what this node knows of a request before it acts on it.
    pub(super) async fn mark(&self, request: &Commitment, state: i64) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| inner.progress(request, state, None).await)?;
        inner.flush()
    }
}
impl<A: RowCommandApplication> Inner<A> {
    /// Verifies the caller and this executor against current policy and
    /// reads the complete state of every invariant scope at `base`.
    async fn command_input(
        &self,
        command: &EncryptedCommand,
        base: &StrongHead,
    ) -> Result<CommandInput, Error> {
        let request = command.verify(self)?;
        if self.progress_of(&request.hash()?).await?.0 == CANCELLED {
            return Err(denied("command was cancelled on this device"));
        }
        let scopes = resources(self, &request)?;
        let grants = self.local_grants(now()?)?;
        let mut state = CommandRows::new();
        for scope in &scopes {
            if !self.can_read_key(*scope, &grants)
                || !grants.iter().any(|grant| {
                    grant.scope == *scope
                        && grant.rows == RowSelection::All
                        && grant.rights.contains(&Right::Execute)
                        && grant
                            .fields
                            .contains(&self.definition.item_id("method", &request.method))
                })
            {
                return Err(denied("complete execution scope denied"));
            }
            let mut head = self
                .conn
                .query(
                    "SELECT sequence,head FROM secure_command_rows WHERE scope=?",
                    [encode(scope)?],
                )
                .await?;
            // Behind the predecessor: retryable once a sync brings the results.
            if let Some(row) = head.next().await? {
                let local = row.get::<i64>(0)? as u64;
                if local < base.sequence {
                    return Err(Error::StrongPending(PendingReason::MissingConfirmedPackage));
                }
                if local != base.sequence || row.get::<Vec<u8>>(1)? != base.head {
                    return Err(denied("executor missing confirmed predecessor"));
                }
            } else if base.sequence != 0 {
                return Err(Error::StrongPending(PendingReason::MissingConfirmedPackage));
            }
            if !self.config.keys.iter().any(|key| key.scope == *scope) {
                return Err(denied("executor missing invariant key"));
            }
            state.insert(*scope, self.command_rows(*scope).await?);
        }
        let invocation: EncryptedPackage = decode(&request.arguments)?;
        let key = self
            .opening_key(invocation.scope, invocation.generation)
            .ok_or_else(|| denied("invocation generation/key unavailable"))?;
        let arguments = invocation.open(&key.key)?;
        // A valid signature does not make the sealed bytes well formed.
        self.app
            .validate_arguments(&request.method, request.version, &arguments)
            .map_err(|message| denied(&message))?;
        Ok(CommandInput {
            request,
            scopes,
            state,
            arguments,
        })
    }
    /// Seals `output` as the immutable preparation of `command` at `base`,
    /// provided the handler's input still holds.
    async fn prepare_command(
        &self,
        command: &EncryptedCommand,
        base: StrongHead,
        decided: &CommandInput,
        output: RowCommandOutput,
    ) -> Result<ConfirmedCommand, Error> {
        let input = self.command_input(command, &base).await?;
        if input != *decided {
            return Err(Error::StrongPending(PendingReason::StaleFence));
        }
        let CommandInput {
            request,
            scopes,
            state,
            ..
        } = input;
        let grants = self.local_grants(now()?)?;
        if output.changes.len() > 32 || output.outputs.len() > 32 {
            return Err(denied("command effect quota"));
        }
        validate_effects(self, &output.changes)?;
        self.app
            .validate_confirmed(&output.changes)
            .map_err(|message| denied(&message))?;
        let caller = self.verify_reference_chain(&command.credentials, command.node, now()?)?;
        let mut after = state.clone();
        let mut changed = BTreeSet::new();
        for change in &output.changes {
            if !changed.insert((change.scope, change.row))
                || !change.permitted_by(&caller)
                || !change.permitted_by(&grants)
            {
                return Err(denied("command row/field effect denied"));
            }
            let rows = after
                .get_mut(&change.scope)
                .ok_or_else(|| denied("command effect outside invariant scopes"))?;
            if rows.get(&change.row) != change.before.as_ref() {
                return Err(Error::Conflict);
            }
            if let Some(fields) = &change.after {
                rows.insert(change.row, fields.clone());
            } else {
                rows.remove(&change.row);
            }
        }
        let mut parts = Vec::new();
        for scope in &scopes {
            let key = self
                .config
                .keys
                .iter()
                .find(|key| key.scope == *scope)
                .ok_or_else(|| denied("result key unavailable"))?;
            parts.push(EncryptedPackage::seal(
                *scope,
                key.generation,
                PackageKind::Rows,
                &key.key,
                &encode(&RowProjection {
                    before: state[scope].clone(),
                    after: after[scope].clone(),
                })?,
            )?);
        }
        for output in output.outputs {
            let scope = output.scope;
            let bytes = output.bytes;
            let kind = match output.kind {
                rocky_core::CommandOutputKind::Result => PackageKind::CommandResult,
                rocky_core::CommandOutputKind::Job => PackageKind::Job,
                rocky_core::CommandOutputKind::Blob => PackageKind::Blob,
                rocky_core::CommandOutputKind::Checkpoint => PackageKind::Checkpoint,
            };
            if !scopes.contains(&scope) {
                return Err(denied("output outside declared command scopes"));
            }
            let key = self
                .config
                .keys
                .iter()
                .find(|key| key.scope == scope)
                .ok_or_else(|| denied("output key unavailable"))?;
            parts.push(EncryptedPackage::seal(
                scope,
                key.generation,
                kind,
                &key.key,
                &bytes,
            )?);
        }
        let package = StrongPackage {
            request,
            sequence: base
                .sequence
                .checked_add(1)
                .ok_or_else(|| denied("strong counter exhausted"))?,
            base,
            rows: PackageSet::bind(&parts)?.to_bytes()?,
            cut_base: Default::default(),
        };
        let mut result = ConfirmedCommand {
            command: command.clone(),
            package: package.encode()?,
            receipt: vec![],
            executor: rocky_credentials::PeerId::of(self.config.node.public_key()),
            credentials: self.config.credentials.clone(),
            signature: self
                .credential_signer
                .as_ref()
                .unwrap_or(&self.config.node)
                .sign(b""),
            parts,
        };
        result.signature = self
            .credential_signer
            .as_ref()
            .unwrap_or(&self.config.node)
            .sign(&result.signature_bytes()?);
        if result.to_bytes()?.len() > MAX_SECURE_FRAME / 2 - 1024 {
            return Err(denied("result must reserve receipt space"));
        }
        Ok(result)
    }
}
