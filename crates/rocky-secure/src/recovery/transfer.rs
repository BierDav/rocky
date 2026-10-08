use super::*;
impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Captures retained protocol artifacts under a read snapshot, never a SQL dump or signing key.
    pub async fn export_recovery(
        &self,
        recipient: PublicKey,
        credentials: &CredentialChain,
        mut scopes: Vec<ResourceScope>,
        recovery_key: &[u8; 32],
        coordinator: &dyn Coordinator,
    ) -> Result<RecoveryPackage, Error> {
        scopes.sort();
        scopes.dedup();
        if scopes.is_empty() || scopes.len() > 32 {
            return Err(denied("recovery scope quota"));
        }
        let mut heads = Vec::new();
        for domain in scopes
            .iter()
            .map(|scope| scope.domain)
            .collect::<BTreeSet<_>>()
        {
            heads.push(coordinator.head(domain).await?);
        }
        let mut inner = self.inner.lock().await;
        // The package leaves the device: flush everything it may capture.
        inner.flush()?;
        let grants = inner.verify_chain(credentials, recipient, now()?)?;
        authorize(&grants, &scopes)?;
        authorize(&inner.local_grants(now()?)?, &scopes)?;
        // A recovery package carries complete operation history. A pruned
        // scope travels as a checkpoint instead.
        for scope in &scopes {
            if inner.has_horizon(*scope).await? || inner.mergeable_horizon(*scope).await?.is_some()
            {
                return Err(denied(
                    "recovery export needs complete history; a pruned scope is transferred as a checkpoint",
                ));
            }
        }
        transaction!(inner, |inner| {
            let mut payload = Payload {
                bases: vec![],
                pruned: vec![],
                frames: vec![],
                results: vec![],
                preparations: vec![],
                commands: vec![],
                handles: vec![],
                observations: vec![],
                durability: vec![],
            };
            for scope in &scopes {
                let mut base = inner.strong_base(*scope).await?;
                if let Some(result) = &mut base.latest {
                    result.parts.retain(|part| scopes.contains(&part.scope));
                }
                payload.bases.push((*scope, base));
            }
            for record in rows(&inner.conn,"SELECT handle,input,node,sequence,domain FROM secure_pruned_commands ORDER BY handle").await? {
                let [SqlValue::Blob(handle),SqlValue::Blob(input),SqlValue::Blob(node),SqlValue::Integer(sequence),SqlValue::Blob(domain)] = record.as_slice() else { return Err(denied("pruned command record")); };
                let domain: Scope = decode(domain)?;
                if scopes.iter().any(|scope| scope.domain == domain) {
                    payload.pruned.push(PrunedCommand { handle:handle.as_slice().try_into().map_err(|_| denied("pruned handle"))?, input:input.as_slice().try_into().map_err(|_| denied("pruned input"))?,node:rocky_core::NodeId(node.as_slice().try_into().map_err(|_| denied("pruned node"))?),sequence:*sequence as u64,domain });
                }
            }
            for record in rows(
                &inner.conn,
                "SELECT header,body FROM secure_operations ORDER BY hash",
            )
            .await?
            {
                let [SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else {
                    return Err(denied("corrupt operation"));
                };
                let frame = EncryptedFrame {
                    header: header.clone(),
                    body: body.clone(),
                };
                if scopes.contains(&frame.scope()?) {
                    payload.frames.push(frame);
                }
            }
            for record in rows(
                &inner.conn,
                "SELECT result FROM secure_results ORDER BY domain,sequence",
            )
            .await?
            {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("corrupt result"));
                };
                let mut result: ConfirmedCommand = decode(bytes)?;
                let domain = result.strong_package()?.request.scope;
                if !scopes.iter().any(|scope| scope.domain == domain) {
                    continue;
                }
                // Invocation ciphertext is part of the signed envelope: require custody of that scope too.
                let request = result.strong_package()?.request;
                let invocation: EncryptedPackage = decode(&request.arguments)?;
                if !scopes.contains(&invocation.scope) {
                    continue;
                }
                result.parts.retain(|part| scopes.contains(&part.scope));
                payload.results.push(result);
            }
            for record in rows(
                &inner.conn,
                "SELECT request,result FROM secure_preparations ORDER BY request",
            )
            .await?
            {
                let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("corrupt preparation"));
                };
                let result: ConfirmedCommand = decode(bytes)?;
                if resources(inner, &result.strong_package()?.request)?
                    .iter()
                    .all(|scope| scopes.contains(scope))
                {
                    payload.preparations.push((key.clone(), result));
                }
            }
            for record in rows(&inner.conn, "SELECT handle,input,command FROM secure_commands UNION ALL SELECT handle,input,command FROM secure_recovered_commands ORDER BY handle").await? {
                let [SqlValue::Blob(handle), SqlValue::Blob(input), SqlValue::Blob(bytes)] = record.as_slice() else { return Err(denied("corrupt command")); };
                let command: EncryptedCommand = decode(bytes)?;
                let invocation: EncryptedPackage = decode(&command.request()?.arguments)?;
                if scopes.contains(&invocation.scope) {
                    payload.commands.push(CommandRecord { handle: handle.as_slice().try_into().map_err(|_| denied("corrupt handle"))?, input: input.as_slice().try_into().map_err(|_| denied("corrupt input"))?, command });
                }
            }
            let ids: BTreeSet<_> = payload
                .frames
                .iter()
                .map(EncryptedFrame::id)
                .collect::<Result<_, _>>()?;
            for record in rows(
                &inner.conn,
                "SELECT handle,input,hash FROM secure_handles ORDER BY handle",
            )
            .await?
            {
                let [
                    SqlValue::Blob(handle),
                    SqlValue::Blob(input),
                    SqlValue::Blob(hash),
                ] = record.as_slice()
                else {
                    return Err(denied("corrupt handle"));
                };
                let operation = Hash::try_from(hash.as_slice())
                    .map_err(|_| denied("corrupt operation hash"))?;
                if ids.contains(&operation) {
                    payload.handles.push(HandleRecord {
                        handle: handle
                            .as_slice()
                            .try_into()
                            .map_err(|_| denied("corrupt handle"))?,
                        input: input
                            .as_slice()
                            .try_into()
                            .map_err(|_| denied("corrupt input"))?,
                        operation,
                    });
                }
            }
            for record in rows(
                &inner.conn,
                "SELECT observation FROM secure_effect_observations ORDER BY id",
            )
            .await?
            {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("corrupt observation"));
                };
                let mut observation: super::super::effects::EffectObservation = decode(bytes)?;
                let invocation: EncryptedPackage =
                    decode(&observation.result.strong_package()?.request.arguments)?;
                if scopes.contains(&observation.scope()) && scopes.contains(&invocation.scope) {
                    observation
                        .result
                        .parts
                        .retain(|part| scopes.contains(&part.scope));
                    payload.observations.push(observation);
                }
            }
            for domain in scopes
                .iter()
                .map(|scope| scope.domain)
                .collect::<BTreeSet<_>>()
            {
                let records = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT policy FROM secure_durability WHERE domain=?",
                            [encode(&domain)?],
                        )
                        .await?,
                )
                .await?;
                if let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) {
                    payload.durability.push((domain, decode(bytes)?));
                }
            }
            let bytes = encode(&payload)?;
            let mut key_epochs = BTreeMap::new();
            // The latest key epoch of every scope; earlier ones stay inside.
            let mut check_epoch = |scope, generation: u64| -> Result<(), Error> {
                let latest = key_epochs.entry(scope).or_insert(generation);
                *latest = (*latest).max(generation);
                Ok(())
            };
            for frame in &payload.frames {
                let op = frame.operation()?;
                check_epoch(op.header.extensions.scope, op.header.extensions.generation)?;
            }
            for result in payload
                .results
                .iter()
                .chain(payload.preparations.iter().map(|(_, result)| result))
            {
                let invocation: EncryptedPackage =
                    decode(&result.strong_package()?.request.arguments)?;
                check_epoch(invocation.scope, invocation.generation)?;
                for part in &result.parts {
                    check_epoch(part.scope, part.generation)?;
                }
            }
            for command in &payload.commands {
                let invocation: EncryptedPackage = decode(&command.command.request()?.arguments)?;
                check_epoch(invocation.scope, invocation.generation)?;
            }
            for (scope, base) in &payload.bases {
                if let Some(signed) = &base.checkpoint {
                    check_epoch(*scope, signed.statement.key_epoch)?;
                }
                if let Some(result) = &base.latest {
                    let invocation: EncryptedPackage =
                        decode(&result.strong_package()?.request.arguments)?;
                    check_epoch(invocation.scope, invocation.generation)?;
                    for part in &result.parts {
                        check_epoch(part.scope, part.generation)?;
                    }
                }
            }
            let inventory = RecoveryInventory {
                format: 2,
                definition: inner.manifest,
                recipient,
                scopes: scopes.clone(),
                schema: inner.definition.schema,
                handler: inner.definition.handler,
                policy: inner.config.policy.clone(),
                key_epochs,
                frontier: ids.into_iter().collect(),
                heads: heads.clone(),
                closure: rocky_core::commitment("rocky.recovery-closure.v2", &bytes),
                revision: inner.revision,
            };
            let mut package = RecoveryPackage {
                ciphertext: super::super::wire::encrypt(
                    recovery_key,
                    &encode(&inventory)?,
                    &bytes,
                )?,
                inventory,
                exporter: inner.config.node.public_key(),
                credentials: inner.config.credentials.clone(),
                signature: inner.config.node.sign(b""),
            };
            package.signature = inner.config.node.sign(&package.signing_bytes()?);
            package.to_bytes()?;
            package.open(recovery_key)?;
            Ok(package)
        })
    }
    /// Import is resumable and idempotent; original signed identities survive as audit records only.
    pub async fn import_recovery(
        &self,
        package: &RecoveryPackage,
        recovery_key: &[u8; 32],
        coordinator: &dyn Coordinator,
    ) -> Result<RecoveryReport, Error> {
        let payload = package.open(recovery_key)?;
        {
            let inner = self.inner.lock().await;
            package.inspect(inner.config.publisher, &inner.config.policy.statement)?;
            if package.inventory.definition != inner.manifest
                || package.inventory.recipient != inner.config.node.public_key()
                || package.exporter == inner.config.node.public_key()
                || package.inventory.schema != inner.definition.schema
                || package.inventory.handler != inner.definition.handler
                || package.inventory.policy.statement.epoch > inner.config.policy.statement.epoch
            {
                return Err(denied(
                    "recovery recipient/definition/policy/generation mismatch",
                ));
            }
            authorize(&inner.local_grants(now()?)?, &package.inventory.scopes)?;
            for scope in &package.inventory.scopes {
                if inner.mergeable_horizon(*scope).await?.is_some() {
                    return Err(denied(
                        "recovery import cannot replace an admitted horizon; use checkpoint rebootstrap",
                    ));
                }
            }
            for (scope, base) in &payload.bases {
                inner.verify_strong_state(*scope, base)?;
            }
            for result in &payload.results {
                result.verify_history(&inner)?;
            }
            for (key, result) in &payload.preparations {
                result.verify_preparation(&inner)?;
                let strong = result.strong_package()?;
                let base = strong.base;
                let expected = rocky_core::commitment(
                    "rocky.encrypted-preparation.v1",
                    &encode(&(
                        strong.request.hash()?,
                        base.configuration,
                        base.executor.0,
                        base.epoch,
                        base.sequence,
                        base.head,
                    ))?,
                );
                if key.as_slice() != expected {
                    return Err(denied("recovery preparation key mismatch"));
                }
            }
            for record in &payload.commands {
                let request = record.command.verify(&inner)?;
                if record.handle != request.client_request {
                    return Err(denied("recovery command deduplication handle mismatch"));
                }
                let invocation: EncryptedPackage = decode(&request.arguments)?;
                if let Some(key) = inner.opening_key(invocation.scope, invocation.generation) {
                    let args = invocation.open(&key.key)?;
                    if rocky_core::commitment(
                        "rocky.command-input.v2",
                        &encode(&(invocation.scope, request.method, request.version, &args))?,
                    ) != record.input
                    {
                        return Err(denied("recovery command input mismatch"));
                    }
                }
            }
        }
        // All external receipt verification and unknown-outcome lookup precede storage writes.
        for result in &payload.results {
            coordinator.verify(&result.receipt()?).await?;
        }
        for (_, base) in &payload.bases {
            if let Some(result) = &base.latest {
                coordinator.verify(&result.receipt()?).await?;
            }
        }
        {
            let mut inner = self.inner.lock().await;
            transaction!(inner, |inner| {
                for (scope, base) in &payload.bases {
                    if base.head.is_some()
                        && inner.strong_base(*scope).await?.sequence() <= base.sequence()
                        && inner.capture_strong(*scope).await?.sequence() <= base.sequence()
                    {
                        inner.install_strong(*scope, base).await?;
                    }
                }
                for record in &payload.pruned {
                    let mut old = inner.conn.query("SELECT input,node,sequence,domain FROM secure_pruned_commands WHERE handle=?", [record.handle.to_vec()]).await?;
                    if let Some(row) = old.next().await?
                        && (row.get::<Vec<u8>>(0)? != record.input
                            || row.get::<Vec<u8>>(1)? != record.node.0
                            || row.get::<i64>(2)? != record.sequence as i64
                            || row.get::<Vec<u8>>(3)? != encode(&record.domain)?)
                    {
                        return Err(Error::Conflict);
                    }
                    inner.conn.execute("INSERT INTO secure_pruned_commands VALUES(?,?,?,?,?) ON CONFLICT(handle) DO NOTHING", bindings(vec![SqlValue::Blob(record.handle.to_vec()),SqlValue::Blob(record.input.to_vec()),SqlValue::Blob(record.node.0.to_vec()),SqlValue::Integer(record.sequence as i64),SqlValue::Blob(encode(&record.domain)?)])).await?;
                    if record.node == secure_node_id(inner.config.node.public_key()) {
                        inner
                            .conn
                            .execute(
                                "UPDATE secure_command_counter SET sequence=MAX(sequence,?)",
                                [record.sequence as i64],
                            )
                            .await?;
                    }
                }
                Ok(())
            })?;
        }
        let mut resolved = Vec::new();
        for (_, candidate) in &payload.preparations {
            let request = candidate.strong_package()?.request;
            // A retired or expired identity has no retained outcome to resolve.
            let established = match coordinator.lookup(request.scope, request.id).await {
                Err(
                    rocky_core::CoordinationError::Retired | rocky_core::CoordinationError::Expired,
                ) => None,
                other => other?,
            };
            if let Some(receipt) = established {
                if candidate.strong_package()?.confirmation()? != receipt.confirmation {
                    return Err(denied("recovered preparation differs from confirmation"));
                }
                let mut result = candidate.clone();
                result.receipt = receipt.encode();
                resolved.push(result);
            }
        }
        for status in self.receive_all(payload.frames.clone()).await {
            status?;
        }
        for result in payload.results.iter().chain(resolved.iter()) {
            self.receive_encrypted_result(result.clone(), coordinator)
                .await?;
        }
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| {
            for record in &payload.handles {
                let old = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT input,hash FROM secure_handles WHERE handle=?",
                            [record.handle.to_vec()],
                        )
                        .await?,
                )
                .await?;
                let expected = vec![
                    SqlValue::Blob(record.input.to_vec()),
                    SqlValue::Blob(record.operation.as_bytes().to_vec()),
                ];
                if old.first().is_some_and(|old| *old != expected) {
                    return Err(Error::Conflict);
                }
                inner
                    .put_handle(&record.handle, &record.input, record.operation.as_bytes())
                    .await?;
            }
            for record in &payload.commands {
                let old = collect_rows(inner.conn.query("SELECT input,command FROM secure_commands WHERE handle=? UNION ALL SELECT input,command FROM secure_recovered_commands WHERE handle=?", [record.handle.to_vec(),record.handle.to_vec()]).await?).await?;
                let expected = vec![
                    SqlValue::Blob(record.input.to_vec()),
                    SqlValue::Blob(record.command.to_bytes()?),
                ];
                if old.first().is_some_and(|old| *old != expected) {
                    return Err(Error::Conflict);
                }
                if old.is_empty() {
                    inner
                        .command_budget(record.command.to_bytes()?.len())
                        .await?;
                }
                let request = record.command.request()?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_tickets VALUES(?,?) ON CONFLICT(handle) DO NOTHING",
                        [record.handle.to_vec(), request.hash()?.to_vec()],
                    )
                    .await?;
                if request.id.node == secure_node_id(inner.config.node.public_key()) {
                    inner
                        .conn
                        .execute(
                            "UPDATE secure_command_counter SET sequence=MAX(sequence,?)",
                            [request.id.sequence.get() as i64],
                        )
                        .await?;
                }
                inner.conn.execute("INSERT INTO secure_recovered_commands VALUES(?,?,?) ON CONFLICT(handle) DO NOTHING", [record.handle.to_vec(), record.input.to_vec(), record.command.to_bytes()?]).await?;
            }
            for (key, result) in &payload.preparations {
                let old = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT result FROM secure_preparations WHERE request=?",
                            [key.clone()],
                        )
                        .await?,
                )
                .await?;
                let expected = vec![SqlValue::Blob(result.to_bytes()?)];
                if old.first().is_some_and(|old| *old != expected) {
                    return Err(Error::Conflict);
                }
                if old.is_empty() {
                    inner.command_budget(result.to_bytes()?.len()).await?;
                }
                inner.conn.execute("INSERT INTO secure_preparations VALUES(?,?) ON CONFLICT(request) DO NOTHING", [key.clone(),result.to_bytes()?]).await?;
                inner.index_preparation(key, result).await?;
            }
            for (domain, policy) in &payload.durability {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_durability VALUES(?,?) ON CONFLICT(domain) DO NOTHING",
                        [encode(domain)?, encode(policy)?],
                    )
                    .await?;
            }
            Ok(())
        })?;
        drop(inner);
        for observation in payload.observations {
            self.receive_effect_observation(observation, coordinator)
                .await?;
        }
        self.recovery_status(&package.inventory.scopes, coordinator)
            .await
    }
    /// Call after takeover as well as after import; confirmation still checks the exact predecessor.
    pub async fn recovery_status(
        &self,
        scopes: &[ResourceScope],
        coordinator: &dyn Coordinator,
    ) -> Result<RecoveryReport, Error> {
        if scopes.is_empty()
            || scopes.len() > 32
            || scopes.iter().collect::<BTreeSet<_>>().len() != scopes.len()
        {
            return Err(denied("recovery scope quota/duplicate"));
        }
        let mut report = RecoveryReport {
            heads: vec![],
            blocked: vec![],
            unresolved: vec![],
        };
        let domains: BTreeSet<_> = scopes.iter().map(|scope| scope.domain).collect();
        for domain in domains {
            let head = coordinator.head(domain).await?;
            {
                let inner = self.inner.lock().await;
                if head.executor != secure_node_id(inner.config.node.public_key()) {
                    report.blocked.push(RecoveryBlock::Fenced(domain));
                }
                if head.schema != inner.definition.schema
                    || head.handler != inner.definition.handler
                {
                    report.blocked.push(RecoveryBlock::Generations(domain));
                }
                for scope in scopes.iter().filter(|scope| scope.domain == domain) {
                    if !inner.config.keys.iter().any(|key| key.scope == *scope) {
                        report.blocked.push(RecoveryBlock::MissingKey(*scope));
                    }
                    let applied = collect_rows(
                        inner
                            .conn
                            .query(
                                "SELECT sequence,head FROM secure_command_rows WHERE scope=?",
                                [encode(scope)?],
                            )
                            .await?,
                    )
                    .await?;
                    let matches = if head.sequence == 0 {
                        applied.is_empty()
                    } else {
                        applied.first()
                            == Some(&vec![
                                SqlValue::Integer(head.sequence as i64),
                                SqlValue::Blob(head.head.to_vec()),
                            ])
                    };
                    if !matches {
                        report.blocked.push(RecoveryBlock::MissingPayload {
                            domain,
                            sequence: head.sequence,
                        });
                    }
                }
            }
            for scope in scopes.iter().filter(|scope| scope.domain == domain) {
                let merged = self.inner.lock().await.scope_complete(*scope).await?;
                if !merged || !self.command_snapshot(*scope).await?.complete {
                    report.blocked.push(RecoveryBlock::IncompleteScope(*scope));
                }
            }
            if coordinator.head(domain).await? != head {
                report.blocked.push(RecoveryBlock::HeadChanged(domain));
            }
            report.heads.push(head);
        }
        let commands = {
            let inner = self.inner.lock().await;
            rows(&inner.conn,"SELECT command FROM secure_commands UNION ALL SELECT command FROM secure_recovered_commands").await?
        };
        for record in commands {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("corrupt command"));
            };
            let command: EncryptedCommand = decode(bytes)?;
            let request = command.request()?;
            // Pending only while admission is open; a settled identity never executes.
            if scopes.iter().any(|scope| scope.domain == request.scope) {
                match coordinator.lookup(request.scope, request.id).await {
                    Ok(None) => report.unresolved.push(command),
                    Ok(Some(_))
                    | Err(
                        rocky_core::CoordinationError::Retired
                        | rocky_core::CoordinationError::Expired,
                    ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(report)
    }
}
