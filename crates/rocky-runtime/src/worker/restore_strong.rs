use super::*;
use rocky_core::*;
use std::collections::{BTreeMap, BTreeSet};

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// Checks the strong ledger's bindings. With `replay`, every applied
    /// result whose sealed cut can still be rebuilt is also recomputed.
    /// Results pruned behind `strong_base` are no longer in the ledger; the
    /// chain of heads is checked from that base.
    pub(super) async fn restore_strong(&self, replay: bool) -> Result<usize, Error> {
        let base = self.retention.strong_base();
        let corrupt = || Error::Corrupt("inconsistent strong ledger".into());
        let meta = rows(self.conn(), "SELECT id,command_sequence,configuration,sequence,head FROM rocky_strong_meta ORDER BY id").await?;
        let [row] = meta.as_slice() else {
            return Err(corrupt());
        };
        let [
            SqlValue::Integer(1),
            SqlValue::Integer(counter),
            configuration,
            SqlValue::Integer(sequence),
            head,
        ] = row.as_slice()
        else {
            return Err(corrupt());
        };
        if *counter < 0 || *sequence < 0 {
            return Err(corrupt());
        }
        let mut requests = BTreeMap::new();
        let (mut local_sequence, mut local_count) = (0, 0);
        for row in rows(
            self.conn(),
            "SELECT node,sequence,bytes FROM rocky_requests ORDER BY node,sequence",
        )
        .await?
        {
            let [
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                SqlValue::Blob(bytes),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let request = StrongRequest::decode(bytes).map_err(|_| corrupt())?;
            if request.scope != self.identity.scope
                || !self
                    .config
                    .authors
                    .contains(&(request.id.node, request.caller))
                || *node != request.id.node.0
                || *sequence as u64 != request.id.sequence.get()
            {
                return Err(corrupt());
            }
            if request.id.node == self.identity.node {
                // Sequences strictly increase; they carry issue times, so they have gaps.
                if request.id.sequence.get() <= local_sequence {
                    return Err(corrupt());
                }
                local_sequence = request.id.sequence.get();
                local_count += 1;
            }
            requests.insert(request.id, request);
        }
        if (*counter as u64) < local_sequence
            || (base.is_none() && *counter as u64 != local_sequence)
        {
            return Err(corrupt());
        }
        let handles = rows(
            self.conn(),
            "SELECT handle,method,node,sequence FROM rocky_command_handles",
        )
        .await?;
        if base.is_none() && handles.len() as u64 != local_count {
            return Err(corrupt());
        }
        let mut handled = BTreeSet::new();
        for row in handles {
            let [
                SqlValue::Blob(handle),
                SqlValue::Text(method),
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let id = RequestId {
                node: NodeId(node.as_slice().try_into().map_err(|_| corrupt())?),
                sequence: AuthorSequence::new(*sequence as u64).map_err(|_| corrupt())?,
            };
            if handle.len() != 32
                || id.node != self.identity.node
                || !handled.insert(id)
                || id.sequence.get() > *counter as u64
                || match requests.get(&id) {
                    Some(request) => {
                        request.method != *method
                            || request.client_request.as_slice() != handle.as_slice()
                    }
                    // A handle outlives its pruned request for retries.
                    None => base.is_none(),
                }
            {
                return Err(corrupt());
            }
        }
        let mut applied = BTreeMap::new();
        let mut applied_hashes = BTreeSet::new();
        for row in rows(
            self.conn(),
            "SELECT hash,bytes,state,receipt,verified,applied FROM rocky_preparations",
        )
        .await?
        {
            let [
                SqlValue::Blob(hash),
                SqlValue::Blob(bytes),
                SqlValue::Text(state),
                receipt,
                SqlValue::Integer(verified),
                SqlValue::Integer(is_applied),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let package = StrongPackage::decode(bytes).map_err(|_| corrupt())?;
            let confirmation = package.confirmation().map_err(|_| corrupt())?;
            if *hash != confirmation.result
                || package.request.scope != self.identity.scope
                || ![0, 1].contains(verified)
                || ![0, 1].contains(is_applied)
                || !["unknown", "confirmed"].contains(&state.as_str())
                || (*is_applied == 1) != (state == "confirmed")
            {
                return Err(corrupt());
            }
            let receipt = match receipt {
                SqlValue::Null if *verified == 0 && *is_applied == 0 => None,
                SqlValue::Blob(bytes) => {
                    let receipt = StrongReceipt::decode(bytes).map_err(|_| corrupt())?;
                    if receipt.confirmation != confirmation || receipt.sequence != package.sequence
                    {
                        return Err(corrupt());
                    }
                    Some(receipt)
                }
                _ => return Err(corrupt()),
            };
            if *verified == 1
                && (requests.get(&package.request.id) != Some(&package.request)
                    || configuration != &SqlValue::Blob(package.base.configuration.to_vec()))
            {
                return Err(corrupt());
            }
            if *is_applied == 1 {
                if *verified != 1 {
                    return Err(corrupt());
                }
                if replay {
                    let replayed = match self.predecessor(&package.base).await {
                        Ok(previous) => {
                            self.replay_cut(&package.request, previous.as_deref()).await
                        }
                        Err(error) => Err(error),
                    };
                    match replayed {
                        Ok((expected, required)) => {
                            if package.rows != expected || package.required != required {
                                return Err(corrupt());
                            }
                        }
                        // The cut or the predecessor is behind the checkpoint.
                        Err(Error::StrongPending(_)) => {}
                        Err(_) => return Err(corrupt()),
                    }
                }
                if applied
                    .insert(package.sequence, (package, receipt.ok_or_else(corrupt)?))
                    .is_some()
                {
                    return Err(corrupt());
                }
                applied_hashes.insert(hash.clone());
            }
        }
        match (configuration, head) {
            (SqlValue::Null, SqlValue::Null)
                if *sequence == 0 && applied.is_empty() && base.is_none() => {}
            (SqlValue::Blob(configuration), SqlValue::Blob(head)) => {
                let configuration: Configuration =
                    configuration.as_slice().try_into().map_err(|_| corrupt())?;
                let (mut expected, mut next) = match base {
                    Some((sequence, head)) => (head, sequence + 1),
                    None => (genesis(self.identity.scope, configuration), 1),
                };
                for (sequence, (package, receipt)) in &applied {
                    if *sequence != next
                        || package.base.head != expected
                        || package.base.configuration != configuration
                    {
                        return Err(corrupt());
                    }
                    expected = receipt.confirmation.new_head;
                    next += 1;
                }
                if *sequence as u64 != next - 1 || *head != expected {
                    return Err(corrupt());
                }
            }
            _ => return Err(corrupt()),
        }
        if let Some((package, _)) = applied.values().next_back() {
            let handler = self
                .app
                .command(&package.request.method)
                .ok_or_else(corrupt)?;
            if let Some(query) = handler.result_projection_query()
                && rows(self.conn(), query).await?
                    != handler
                        .result_projection_rows(&package.rows)
                        .map_err(|_| corrupt())?
            {
                return Err(Error::Corrupt(
                    "strong projection/job intents differ from confirmed replay".into(),
                ));
            }
        } else {
            for method in self.app.commands() {
                let handler = self.app.command(method.name).ok_or_else(corrupt)?;
                if let Some(query) = handler.result_projection_query()
                    && !rows(self.conn(), query).await?.is_empty()
                {
                    return Err(corrupt());
                }
            }
        }
        let mut acknowledgments = BTreeSet::new();
        for row in rows(
            self.conn(),
            "SELECT peer,hash,applied FROM rocky_strong_ack",
        )
        .await?
        {
            let [
                SqlValue::Blob(peer),
                SqlValue::Blob(hash),
                SqlValue::Integer(applied),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            self.validate_peer(NodeId(peer.as_slice().try_into().map_err(|_| corrupt())?))
                .map_err(|_| corrupt())?;
            if !applied_hashes.contains(hash) || ![0, 1].contains(applied) {
                return Err(corrupt());
            }
            if *applied == 1 {
                acknowledgments.insert((peer.clone(), hash.clone()));
            }
        }
        let delivered: BTreeSet<_> = applied_hashes
            .iter()
            .filter(|hash| {
                self.config
                    .delivery_targets
                    .iter()
                    .all(|peer| acknowledgments.contains(&(peer.0.to_vec(), (*hash).clone())))
            })
            .cloned()
            .collect();
        let expected: Vec<_> = applied_hashes
            .difference(&delivered)
            .map(|hash| vec![SqlValue::Blob(hash.clone())])
            .collect();
        if rows(
            self.conn(),
            "SELECT hash FROM rocky_strong_outbox ORDER BY hash",
        )
        .await?
            != expected
        {
            return Err(corrupt());
        }
        Ok(applied.len())
    }
}
