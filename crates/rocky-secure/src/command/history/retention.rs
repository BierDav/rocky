//! Domain acknowledgments and bounded pruning batches.
use super::*;

impl<A: RowApplication> Inner<A> {
    /// Conservative domain cut: every configured scope and required reader must cover it.
    async fn strong_horizon(&self, domain: Scope) -> Result<u64, Error> {
        let mut through = u64::MAX;
        let mut window = 0;
        let mut found = false;
        for record in rows(&self.conn, "SELECT scope,sequence FROM secure_command_rows").await? {
            let [SqlValue::Blob(scope), SqlValue::Integer(sequence)] = record.as_slice() else {
                return Err(denied("strong projection"));
            };
            let scope: ResourceScope = decode(scope)?;
            if scope.domain != domain {
                continue;
            }
            let Some(policy) = self.retention_policy(scope).await? else {
                return Ok(0);
            };
            found = true;
            window = window.max(policy.window as u64);
            through = through.min(*sequence as u64);
            for peer in policy.required.difference(&policy.behind) {
                let mut applied = self.conn.query("SELECT COALESCE(MAX(sequence),0) FROM secure_result_ack WHERE domain=? AND scope=? AND peer=? AND applied=1", [encode(&domain)?,encode(&scope)?,peer.as_bytes().to_vec()]).await?;
                through = through.min(
                    applied
                        .next()
                        .await?
                        .ok_or_else(|| denied("result acknowledgment"))?
                        .get::<i64>(0)? as u64,
                );
            }
        }
        let grants = self.local_grants(now()?)?;
        let mut latest = self
            .conn
            .query(
                "SELECT result FROM secure_results WHERE domain=? ORDER BY sequence DESC LIMIT 1",
                [encode(&domain)?],
            )
            .await?;
        if let Some(row) = latest.next().await? {
            let result: ConfirmedCommand = decode(&row.get::<Vec<u8>>(0)?)?;
            for part in result
                .parts
                .iter()
                .filter(|part| part.kind == PackageKind::Rows)
            {
                if self.can_read_key(part.scope, &grants)
                    && self.config.keys.iter().any(|key| key.scope == part.scope)
                {
                    let mut applied = self
                        .conn
                        .query(
                            "SELECT sequence FROM secure_command_rows WHERE scope=?",
                            [encode(&part.scope)?],
                        )
                        .await?;
                    if applied.next().await?.is_none() {
                        return Ok(0);
                    }
                }
            }
        }
        Ok(if found {
            through.saturating_sub(window)
        } else {
            0
        })
    }
    pub(crate) async fn prune_strong(&mut self, domain: Scope) -> Result<(), Error> {
        let policies = rows(&self.conn, "SELECT scope FROM secure_retention").await?;
        if !policies.iter().any(|record| matches!(record.as_slice(), [SqlValue::Blob(bytes)] if decode::<ResourceScope>(bytes).is_ok_and(|scope| scope.domain == domain))) { return Ok(()); }
        let through = self.strong_horizon(domain).await?;
        let blob = encode(&domain)?;
        transaction!(self, |inner| {
            let mut changed = false;
            inner.conn.execute("INSERT INTO secure_strong_horizon VALUES(?,?) ON CONFLICT(domain) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Integer(through as i64)])).await?;
            // Redundant and stale attempts no longer hold a second full payload.
            for record in collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT s.request,s.result FROM secure_preparations s JOIN secure_preparation_requests i ON i.preparation=s.request JOIN secure_command_progress p ON p.request=i.request WHERE p.state IN (3,4,5,6) ORDER BY s.request LIMIT 64",
                        (),
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("preparation"));
                };
                let result: ConfirmedCommand = decode(bytes)?;
                let request = result.strong_package()?.request;
                let mut progress = inner
                    .conn
                    .query(
                        "SELECT state FROM secure_command_progress WHERE request=?",
                        [request.hash()?.to_vec()],
                    )
                    .await?;
                if progress.next().await?.is_some_and(|row| {
                    row.get::<i64>(0)
                        .is_ok_and(|state| [3, 4, 5, 6].contains(&state))
                }) {
                    changed = true;
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_preparations WHERE request=?",
                            [key.clone()],
                        )
                        .await?;
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_preparation_requests WHERE preparation=?",
                            [key.clone()],
                        )
                        .await?;
                }
            }
            for record in collect_rows(inner.conn.query("SELECT c.handle,c.input,c.command FROM (SELECT handle,input,command,sequence FROM secure_commands UNION ALL SELECT handle,input,command,0 FROM secure_recovered_commands) c JOIN secure_tickets t ON t.handle=c.handle JOIN secure_command_progress p ON p.request=t.request WHERE p.state IN (3,4,5,6) AND (p.state!=4 OR (p.sequence<=? AND p.sequence<(SELECT MAX(sequence) FROM secure_results WHERE domain=?))) ORDER BY c.sequence LIMIT 64", bindings(vec![SqlValue::Integer(through as i64),SqlValue::Blob(blob.clone())])).await?).await? {
                let [SqlValue::Blob(handle),SqlValue::Blob(input),SqlValue::Blob(bytes)] = record.as_slice() else { return Err(denied("settled command")); };
                let request = EncryptedCommand::from_bytes(bytes)?.request()?;
                if request.scope != domain { continue; }
                changed = true;
                inner.conn.execute("INSERT INTO secure_pruned_commands VALUES(?,?,?,?,?) ON CONFLICT(handle) DO NOTHING", bindings(vec![SqlValue::Blob(handle.clone()),SqlValue::Blob(input.clone()),SqlValue::Blob(request.id.node.0.to_vec()),SqlValue::Integer(request.id.sequence.get() as i64),SqlValue::Blob(encode(&request.scope)?)])).await?;
                inner.conn.execute("DELETE FROM secure_commands WHERE handle=?", [handle.clone()]).await?;
                inner.conn.execute("DELETE FROM secure_recovered_commands WHERE handle=?", [handle.clone()]).await?;
            }
            let mut cursor = inner
                .conn
                .query(
                    "SELECT sequence FROM secure_strong_prune_cursor WHERE domain=?",
                    [blob.clone()],
                )
                .await?;
            let cursor = cursor
                .next()
                .await?
                .map(|row| row.get::<i64>(0))
                .transpose()?
                .unwrap_or(0);
            let candidates = collect_rows(inner.conn.query("SELECT sequence,result FROM secure_results WHERE domain=? AND sequence>? AND sequence<=? AND sequence<(SELECT MAX(sequence) FROM secure_results WHERE domain=?) ORDER BY sequence LIMIT 64", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Integer(cursor),SqlValue::Integer(through as i64),SqlValue::Blob(blob.clone())])).await?).await?;
            let mut scanned = 0;
            for record in candidates {
                let [SqlValue::Integer(sequence), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("strong prune candidate"));
                };
                scanned = *sequence;
                let result: ConfirmedCommand = decode(bytes)?;
                let package = result.strong_package()?;
                // A reader retains jobs only in the scopes whose parts it holds.
                let binding = PackageSet::from_bytes(&package.rows)?;
                let mut observed = true;
                for part in result
                    .parts
                    .iter()
                    .filter(|part| part.kind == PackageKind::Job)
                {
                    let index = binding
                        .commitments
                        .iter()
                        .position(|id| part.commitment().ok().as_ref() == Some(id))
                        .ok_or_else(|| denied("job binding"))?;
                    let job = rocky_core::commitment(
                        "rocky.effect-job.v1",
                        &encode(&(package.request.hash()?, index as u64))?,
                    );
                    let mut settled = false;
                    let records = collect_rows(
                        inner
                            .conn
                            .query(
                                "SELECT observation FROM secure_effect_observations WHERE job=?",
                                [job.to_vec()],
                            )
                            .await?,
                    )
                    .await?;
                    for record in records {
                        if let [SqlValue::Blob(bytes)] = record.as_slice() {
                            settled |= decode::<crate::effects::EffectObservation>(bytes)?
                                .settled(inner)?;
                        }
                    }
                    observed &= settled;
                }
                if !observed {
                    continue;
                }
                // Advance each readable scope's base using the authenticated after-rows.
                let mut safe = true;
                let scopes = rows(&inner.conn, "SELECT scope FROM secure_command_rows").await?;
                for record in scopes {
                    let [SqlValue::Blob(bytes)] = record.as_slice() else {
                        return Err(denied("command scope"));
                    };
                    let scope: ResourceScope = decode(bytes)?;
                    if scope.domain != domain {
                        continue;
                    }
                    let base = inner.strong_base(scope).await?;
                    if base.sequence() >= *sequence as u64 {
                        continue;
                    }
                    let Some(part) = result
                        .parts
                        .iter()
                        .find(|part| part.scope == scope && part.kind == PackageKind::Rows)
                    else {
                        safe = false;
                        break;
                    };
                    let Some(key) = inner.opening_key(scope, part.generation) else {
                        safe = false;
                        break;
                    };
                    let projection: RowProjection = decode(&part.open(&key.key)?)?;
                    let state = StrongState {
                        head: Some((*sequence as u64, result.receipt()?.confirmation.new_head)),
                        rows: projection.after,
                        latest: Some(result.clone()),
                        checkpoint: None,
                    };
                    changed = true;
                    inner.put_strong_base(scope, &state).await?;
                }
                if !safe {
                    break;
                }
                changed = true;
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_results WHERE domain=? AND sequence=?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Integer(*sequence),
                        ]),
                    )
                    .await?;
                // Keep the newest per-peer report as the monotonic application watermark.
                inner.conn.execute("DELETE FROM secure_result_ack WHERE domain=? AND sequence<? AND EXISTS(SELECT 1 FROM secure_result_ack newer WHERE newer.domain=secure_result_ack.domain AND newer.peer=secure_result_ack.peer AND newer.scope=secure_result_ack.scope AND newer.applied>=secure_result_ack.applied AND newer.sequence>secure_result_ack.sequence)", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Integer(*sequence)])).await?;
            }
            inner.conn.execute("INSERT INTO secure_strong_prune_cursor VALUES(?,?) ON CONFLICT(domain) DO UPDATE SET sequence=excluded.sequence", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Integer(scanned)])).await?;
            if changed {
                inner.fault(crate::replica::checkpoint::Stage::Prune)?;
            }
            Ok(())
        })
    }
}
