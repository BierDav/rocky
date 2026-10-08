//! Durable command tickets: submission and progress lookup are local and
//! separate from whatever delivers a command to its executor. See
//! `docs/runtime/typed-commands.md`.
use super::*;

/// Unresolved tickets one device holds; a further submission is refused
/// until one resolves or is cancelled.
pub const MAX_OPEN_TICKETS: usize = 64;

pub(super) const PREPARED: i64 = 1;
pub(super) const OUTCOME_UNKNOWN: i64 = 2;
pub(super) const CANCELLED: i64 = 3;
pub(super) const CONFIRMED: i64 = 4;
pub(super) const RETIRED: i64 = 5;
pub(super) const EXPIRED: i64 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketState {
    /// Stored on this device; nothing left it and nothing was prepared.
    Queued,
    /// This device prepared the transition and has not submitted it.
    Prepared,
    /// The command left this device, or a confirmation was submitted, and
    /// no answer established what happened. Only the same ticket resolves it.
    OutcomeUnknown,
    /// The coordinator's receipt is verified and the result retained. Rows
    /// this device may read are not applied yet, or it reads none.
    Confirmed,
    /// Confirmed, and every scope this device reads is applied locally.
    Applied,
    /// Withdrawn before it left this device. It is never sent or executed.
    Cancelled,
    /// The coordinator retired its identity: it never executes again, and
    /// whether it was confirmed is unavailable there.
    Retired,
    /// Its admission window closed unconfirmed: it was never executed.
    Expired,
}
/// A peer's acknowledgment for one scope of a result: custody of its
/// ciphertext, or also its application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Custody {
    pub peer: PublicKey,
    pub scope: ResourceScope,
    pub applied: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TicketResult {
    /// Not confirmed yet.
    Pending,
    /// Confirmed, but this device does not hold or cannot read the result's
    /// scope. The receipt alone grants no access to it.
    Unavailable,
    /// The result bytes the handler returned, opened with the scope's key.
    Available(Vec<u8>),
}
/// What one device knows of a command it submitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub handle: Commitment,
    pub request: rocky_core::RequestId,
    pub request_hash: Commitment,
    pub method: String,
    pub version: u32,
    pub state: TicketState,
    /// The confirmed position in the invariant domain.
    pub sequence: Option<u64>,
    pub result: TicketResult,
    pub custody: Vec<Custody>,
}
/// An executor's answer to a delivered command. It reports whether a
/// confirmation was submitted, so a caller knows when the outcome is open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Delivery {
    /// Confirmed; the parts are those the command's caller may hold.
    Confirmed(Box<ConfirmedCommand>),
    /// Refused or failed before any confirmation was submitted.
    NotSubmitted { retryable: bool, message: String },
    /// A confirmation was submitted and its outcome is not established.
    Unknown(String),
    /// The coordinator settled the identity without a retained outcome: it
    /// never executes. `expired` when it was never confirmed; otherwise
    /// whether it was is unavailable.
    Settled { expired: bool },
}
impl Delivery {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_SECURE_FRAME {
            return Err(denied("command delivery quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode(bytes)
    }
}

impl<A: rocky_core::RowApplication> Inner<A> {
    /// The recorded state of a request and its confirmed sequence; 0 when
    /// nothing is recorded.
    pub(crate) async fn progress_of(
        &self,
        request: &Commitment,
    ) -> Result<(i64, Option<u64>), Error> {
        let mut found = self
            .conn
            .query(
                "SELECT state,COALESCE(sequence,0) FROM secure_command_progress WHERE request=?",
                [request.to_vec()],
            )
            .await?;
        Ok(match found.next().await? {
            Some(row) => (
                row.get::<i64>(0)?,
                Some(row.get::<i64>(1)? as u64).filter(|sequence| *sequence > 0),
            ),
            None => (0, None),
        })
    }
    /// Advances a request inside the caller's transaction. A confirmed
    /// request stays confirmed and a cancelled one cancelled.
    pub(super) async fn progress(
        &self,
        request: &Commitment,
        state: i64,
        sequence: Option<u64>,
    ) -> Result<(), Error> {
        let sql = match state {
            PREPARED => {
                "INSERT INTO secure_command_progress VALUES(?,?,?) ON CONFLICT(request) DO NOTHING"
            }
            OUTCOME_UNKNOWN => {
                "INSERT INTO secure_command_progress VALUES(?,?,?) ON CONFLICT(request) DO UPDATE SET state=excluded.state WHERE secure_command_progress.state=1"
            }
            // A settled identity never overrides a confirmation or cancellation.
            RETIRED | EXPIRED => {
                "INSERT INTO secure_command_progress VALUES(?,?,?) ON CONFLICT(request) DO UPDATE SET state=excluded.state WHERE secure_command_progress.state NOT IN (3,4)"
            }
            _ => {
                "INSERT INTO secure_command_progress VALUES(?,?,?) ON CONFLICT(request) DO UPDATE SET state=excluded.state,sequence=excluded.sequence"
            }
        };
        self.conn
            .execute(
                sql,
                bindings(vec![
                    SqlValue::Blob(request.to_vec()),
                    SqlValue::Integer(state),
                    sequence.map_or(SqlValue::Null, |sequence| {
                        SqlValue::Integer(sequence as i64)
                    }),
                ]),
            )
            .await?;
        Ok(())
    }
    /// An explicit refusal: the submitted confirmation was not accepted.
    pub(super) async fn unsubmitted(&self, request: &Commitment) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE secure_command_progress SET state=1 WHERE request=? AND state=2",
                [request.to_vec()],
            )
            .await?;
        Ok(())
    }
}
impl<A: RowCommandApplication> Inner<A> {
    pub(super) async fn open_tickets(&self) -> Result<usize, Error> {
        let mut count = self
            .conn
            .query(
                "SELECT COUNT(*) FROM secure_commands c JOIN secure_tickets t ON t.handle=c.handle LEFT JOIN secure_command_progress p ON p.request=t.request WHERE COALESCE(p.state,0) NOT IN (3,4,5,6)",
                (),
            )
            .await?;
        Ok(count
            .next()
            .await?
            .ok_or_else(|| denied("missing ticket count"))?
            .get::<i64>(0)? as usize)
    }
    async fn submitted(&self, handle: &Commitment) -> Result<Option<EncryptedCommand>, Error> {
        self.check_pruned_handle(*handle, None).await?;
        let retained = collect_rows(
            self.conn
                .query(
                    "SELECT command FROM secure_commands WHERE handle=? UNION ALL SELECT command FROM secure_recovered_commands WHERE handle=?",
                    [handle.to_vec(), handle.to_vec()],
                )
                .await?,
        )
        .await?;
        match retained.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes)]) => Ok(Some(decode(bytes)?)),
            Some(_) => Err(denied("corrupt retained command")),
            None => Ok(None),
        }
    }
    async fn ticket(&self, handle: Commitment) -> Result<Option<Ticket>, Error> {
        let Some(command) = self.submitted(&handle).await? else {
            return Ok(None);
        };
        let request = command.request()?;
        let request_hash = request.hash()?;
        let (recorded, sequence) = self.progress_of(&request_hash).await?;
        let mut ticket = Ticket {
            handle,
            request: request.id,
            request_hash,
            method: request.method.clone(),
            version: request.version,
            state: match recorded {
                PREPARED => TicketState::Prepared,
                OUTCOME_UNKNOWN => TicketState::OutcomeUnknown,
                CANCELLED => TicketState::Cancelled,
                CONFIRMED => TicketState::Confirmed,
                RETIRED => TicketState::Retired,
                EXPIRED => TicketState::Expired,
                _ => TicketState::Queued,
            },
            sequence: None,
            result: TicketResult::Pending,
            custody: Vec::new(),
        };
        let (CONFIRMED, Some(sequence)) = (recorded, sequence) else {
            return Ok(Some(ticket));
        };
        let domain = encode(&request.scope)?;
        let position = || {
            bindings(vec![
                SqlValue::Blob(domain.clone()),
                SqlValue::Integer(sequence as i64),
            ])
        };
        let mut retained = self
            .conn
            .query(
                "SELECT result FROM secure_results WHERE domain=? AND sequence=?",
                position(),
            )
            .await?;
        let Some(row) = retained.next().await? else {
            return Err(denied("confirmed ticket lost its retained result"));
        };
        let result: ConfirmedCommand = decode(&row.get::<Vec<u8>>(0)?)?;
        if result.strong_package()?.request != request {
            return Err(denied("ticket result names another request"));
        }
        ticket.sequence = Some(sequence);
        let grants = self.local_grants(now()?)?;
        let mut readable = 0;
        let mut applied = 0;
        for scope in resources(self, &request)? {
            if !self.can_read_key(scope, &grants)
                || !self.config.keys.iter().any(|key| key.scope == scope)
            {
                continue;
            }
            readable += 1;
            let mut head = self
                .conn
                .query(
                    "SELECT sequence FROM secure_command_rows WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            if let Some(row) = head.next().await?
                && row.get::<i64>(0)? as u64 >= sequence
            {
                applied += 1;
            }
        }
        if readable > 0 && readable == applied {
            ticket.state = TicketState::Applied;
        }
        ticket.result = TicketResult::Unavailable;
        if let Some(part) = result
            .parts
            .iter()
            .find(|part| part.kind == PackageKind::CommandResult)
            && self.can_read_key(part.scope, &grants)
            && let Some(key) = self.opening_key(part.scope, part.generation)
        {
            ticket.result = TicketResult::Available(part.open(&key.key)?);
        }
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT peer,scope,MAX(applied) FROM secure_result_ack WHERE domain=? AND sequence>=? GROUP BY peer,scope ORDER BY peer,scope",
                    position(),
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Blob(peer),
                SqlValue::Blob(scope),
                SqlValue::Integer(applied),
            ] = record.as_slice()
            else {
                return Err(denied("corrupt result acknowledgment"));
            };
            ticket.custody.push(Custody {
                peer: PublicKey::try_from(peer.as_slice())
                    .map_err(|_| denied("corrupt result acknowledgment"))?,
                scope: decode(scope)?,
                applied: *applied != 0,
            });
        }
        Ok(Some(ticket))
    }
    /// The parts of a result the command's caller may hold.
    fn for_caller(&self, result: &ConfirmedCommand) -> Result<ConfirmedCommand, Error> {
        let grants = self.verify_chain(&result.command.credentials, result.command.node, now()?)?;
        let mut filtered = result.clone();
        filtered.parts.retain(|part| {
            grants.iter().any(|grant| {
                grant.scope == part.scope
                    && grant.rows == RowSelection::All
                    && (grant.rights.contains(&Right::Read)
                        || grant.rights.contains(&Right::Custody))
            })
        });
        Ok(filtered)
    }
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Stores the signed invocation and returns its ticket. The same handle
    /// with the same input returns the same ticket; another input conflicts.
    /// Nothing is sent: delivery and execution are separate calls.
    pub async fn submit_command(
        &self,
        handle: Commitment,
        domain: rocky_core::Scope,
        method: &str,
        version: u32,
        arguments: &[u8],
    ) -> Result<Ticket, Error> {
        let scope = self
            .inner
            .lock()
            .await
            .app
            .command_invocation_scope(domain, method)
            .map_err(|message| denied(&message))?;
        self.invoke_version(
            handle,
            scope,
            method,
            version,
            arguments,
            Some(MAX_OPEN_TICKETS),
        )
        .await?;
        self.ticket(handle)
            .await?
            .ok_or_else(|| denied("submitted ticket is missing"))
    }
    /// What this device knows of a submitted command. None: no such handle
    /// was committed here.
    pub async fn ticket(&self, handle: Commitment) -> Result<Option<Ticket>, Error> {
        self.inner.lock().await.ticket(handle).await
    }
    /// Unresolved tickets in submission order, at most `limit`.
    pub async fn open_tickets(&self, limit: usize) -> Result<Vec<Ticket>, Error> {
        let inner = self.inner.lock().await;
        let handles = collect_rows(
            inner
                .conn
                .query(
                    "SELECT t.handle FROM secure_tickets t JOIN secure_commands c ON c.handle=t.handle LEFT JOIN secure_command_progress p ON p.request=t.request WHERE COALESCE(p.state,0) NOT IN (3,4,5,6) ORDER BY c.sequence LIMIT ?",
                    [limit.min(MAX_OPEN_TICKETS) as i64],
                )
                .await?,
        )
        .await?;
        let mut tickets = Vec::new();
        for record in handles {
            let [SqlValue::Blob(handle)] = record.as_slice() else {
                return Err(denied("corrupt ticket"));
            };
            let handle: Commitment = handle
                .as_slice()
                .try_into()
                .map_err(|_| denied("corrupt ticket"))?;
            tickets.extend(inner.ticket(handle).await?);
        }
        Ok(tickets)
    }
    /// Withdraws a ticket that has not left this device. One that was sent
    /// or submitted for confirmation is returned unchanged: a confirmed
    /// transition cannot be rolled back, only waited for.
    pub async fn cancel_ticket(&self, handle: Commitment) -> Result<Ticket, Error> {
        let mut inner = self.inner.lock().await;
        let ticket = inner
            .ticket(handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))?;
        if !matches!(ticket.state, TicketState::Queued | TicketState::Prepared) {
            return Ok(ticket);
        }
        transaction!(inner, |inner| {
            // Checked again inside the transaction that records it.
            if inner.progress_of(&ticket.request_hash).await?.0 > PREPARED {
                return Ok(());
            }
            inner.progress(&ticket.request_hash, CANCELLED, None).await
        })?;
        inner
            .ticket(handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))
    }
    /// The signed invocation of a ticket, for a transport or gateway to
    /// forward. From here the outcome is unknown until an answer settles it.
    pub async fn export_ticket(&self, handle: Commitment) -> Result<EncryptedCommand, Error> {
        let mut inner = self.inner.lock().await;
        let command = inner
            .submitted(&handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))?;
        let request = command.verify(&inner)?.hash()?;
        transaction!(inner, |inner| {
            if inner.progress_of(&request).await?.0 == CANCELLED {
                return Err(denied("ticket was cancelled"));
            }
            inner.progress(&request, OUTCOME_UNKNOWN, None).await
        })?;
        inner.flush()?;
        Ok(command)
    }
    /// The one admission check of a command, whether this device submitted
    /// it or a transport delivered it: signature, current credentials,
    /// registered method, supported version and invocation scope.
    pub async fn admit_command(&self, bytes: &[u8]) -> Result<EncryptedCommand, Error> {
        let command = EncryptedCommand::from_bytes(bytes)?;
        command.verify(&*self.inner.lock().await)?;
        Ok(command)
    }
    /// Executes a ticket this device submitted, if it is the executor.
    pub async fn run_ticket(
        &self,
        handle: Commitment,
        coordinator: &dyn Coordinator,
    ) -> Result<Ticket, Error> {
        let command = {
            let inner = self.inner.lock().await;
            let command = inner
                .submitted(&handle)
                .await?
                .ok_or_else(|| denied("unknown ticket"))?;
            match inner.progress_of(&command.request()?.hash()?).await?.0 {
                CANCELLED => return Err(denied("ticket was cancelled")),
                // Settled already, here or through another executor.
                CONFIRMED => {
                    return inner
                        .ticket(handle)
                        .await?
                        .ok_or_else(|| denied("unknown ticket"));
                }
                _ => command,
            }
        };
        let command = self.admit_command(&command.to_bytes()?).await?;
        let request = command.request()?.hash()?;
        // A settled identity is an answer, not a failure: the ticket shows it.
        if let Err(error) = self.execute_encrypted(command, coordinator).await
            && !self.settle_identity(&request, &error).await?
        {
            return Err(error);
        }
        self.ticket(handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))
    }
    /// Tells the coordinator that every command this device submitted
    /// through the settled prefix is resolved, so it may retire those
    /// identities and free their response slots. Readers verify results
    /// online: call this once the readers that need a result received it.
    /// Until then the coordinator keeps each response for its horizon.
    pub async fn acknowledge_tickets(&self, coordinator: &dyn Coordinator) -> Result<(), Error> {
        let (node, prefixes) = {
            let mut inner = self.inner.lock().await;
            inner.flush()?;
            // Commands through the cursor are settled for good and not read again.
            let (mut cursor, settled) = inner.acknowledged.clone();
            let records = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT c.command,COALESCE(p.state,0),c.sequence,NULL FROM secure_commands c JOIN secure_tickets t ON t.handle=c.handle LEFT JOIN secure_command_progress p ON p.request=t.request WHERE c.sequence>? UNION ALL SELECT NULL,4,sequence,domain FROM secure_pruned_commands WHERE sequence>? AND node=? ORDER BY 3",
                        bindings(vec![SqlValue::Integer(cursor),SqlValue::Integer(cursor),SqlValue::Blob(secure_node_id(inner.config.node.public_key()).0.to_vec())]),
                    )
                    .await?,
            )
            .await?;
            let mut prefixes: BTreeMap<rocky_core::Scope, (u64, bool)> = settled
                .iter()
                .map(|(scope, through)| (*scope, (*through, true)))
                .collect();
            let mut settled = Some(settled);
            for record in records {
                let [
                    bytes,
                    SqlValue::Integer(state),
                    SqlValue::Integer(sequence),
                    domain,
                ] = record.as_slice()
                else {
                    return Err(denied("corrupt ticket"));
                };
                let (domain, request_sequence) = match (bytes, domain) {
                    (SqlValue::Blob(bytes), _) => {
                        let request = decode::<EncryptedCommand>(bytes)?.request()?;
                        (request.scope, request.id.sequence.get())
                    }
                    (SqlValue::Null, SqlValue::Blob(domain)) => {
                        (decode::<Scope>(domain)?, *sequence as u64)
                    }
                    _ => return Err(denied("corrupt settled ticket")),
                };
                let terminal = [CANCELLED, CONFIRMED, RETIRED, EXPIRED].contains(state);
                let prefix = prefixes.entry(domain).or_insert((0, true));
                if prefix.1 && terminal {
                    prefix.0 = request_sequence;
                } else {
                    prefix.1 = false;
                }
                match settled.as_mut().filter(|_| terminal) {
                    Some(settled) => {
                        settled.insert(domain, request_sequence);
                        cursor = *sequence;
                    }
                    None => settled = None,
                }
            }
            if let Some(settled) = settled {
                inner.acknowledged = (cursor, settled);
            }
            (secure_node_id(inner.config.node.public_key()), prefixes)
        };
        for (scope, (through, _)) in prefixes {
            if through > 0 {
                coordinator.retire(scope, node, through).await?;
            }
        }
        Ok(())
    }
    /// Records a coordinator answer that settles an identity for good.
    async fn settle_identity(&self, request: &Commitment, error: &Error) -> Result<bool, Error> {
        let state = match error {
            Error::Coordination(rocky_core::CoordinationError::Retired) => RETIRED,
            Error::Coordination(rocky_core::CoordinationError::Expired) => EXPIRED,
            _ => return Ok(false),
        };
        let mut inner = self.inner.lock().await;
        transaction!(inner, |inner| inner.progress(request, state, None).await)?;
        inner.flush()?;
        Ok(true)
    }
    /// Admits and executes a delivered command. Never fails: the answer
    /// tells the caller whether a confirmation was submitted.
    pub async fn deliver_command(&self, bytes: &[u8], coordinator: &dyn Coordinator) -> Delivery {
        let refused = |error: Error| Delivery::NotSubmitted {
            retryable: error.is_retryable(),
            message: error.to_string(),
        };
        let command = match self.admit_command(bytes).await {
            Ok(command) => command,
            Err(error) => return refused(error),
        };
        let request = match command.request().and_then(|request| Ok(request.hash()?)) {
            Ok(request) => request,
            Err(error) => return refused(error),
        };
        match self.execute_encrypted(command, coordinator).await {
            Ok(result) => match self.inner.lock().await.for_caller(&result) {
                Ok(result) => Delivery::Confirmed(Box::new(result)),
                Err(error) => Delivery::Unknown(error.to_string()),
            },
            Err(Error::Coordination(rocky_core::CoordinationError::Retired)) => {
                Delivery::Settled { expired: false }
            }
            Err(Error::Coordination(rocky_core::CoordinationError::Expired)) => {
                Delivery::Settled { expired: true }
            }
            Err(error) => {
                let recorded = self.inner.lock().await.progress_of(&request).await;
                match recorded {
                    Ok((state, _)) if state < OUTCOME_UNKNOWN => refused(error),
                    _ => Delivery::Unknown(error.to_string()),
                }
            }
        }
    }
    /// Applies an executor's answer to the ticket it was delivered for.
    pub async fn settle_delivery(
        &self,
        handle: Commitment,
        delivery: Delivery,
        coordinator: &dyn Coordinator,
    ) -> Result<Ticket, Error> {
        let ticket = self
            .ticket(handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))?;
        match delivery {
            Delivery::Confirmed(result) => {
                if result.strong_package()?.request.hash()? != ticket.request_hash {
                    return Err(denied("delivered result names another request"));
                }
                self.receive_encrypted_result(*result, coordinator).await?;
            }
            // Nothing was submitted: the ticket can be delivered again.
            Delivery::NotSubmitted { .. } => {
                let mut inner = self.inner.lock().await;
                transaction!(inner, |inner| {
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_command_progress WHERE request=? AND state=2",
                            [ticket.request_hash.to_vec()],
                        )
                        .await?;
                    Ok(())
                })?;
            }
            Delivery::Unknown(_) => {}
            Delivery::Settled { expired } => {
                let error = if expired {
                    rocky_core::CoordinationError::Expired
                } else {
                    rocky_core::CoordinationError::Retired
                };
                self.settle_identity(&ticket.request_hash, &Error::Coordination(error))
                    .await?;
            }
        }
        self.ticket(handle)
            .await?
            .ok_or_else(|| denied("unknown ticket"))
    }
}
