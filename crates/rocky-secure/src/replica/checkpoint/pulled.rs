use super::*;
use rocky_core::Scope;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_pulled(scope INTEGER PRIMARY KEY,seal BLOB NOT NULL,signed BLOB NOT NULL,sets BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_pulled_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row))",
    "CREATE TABLE IF NOT EXISTS secure_pulled_stubs(scope INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_pulled_pages(scope INTEGER NOT NULL,after BLOB NOT NULL,next BLOB,signature BLOB NOT NULL,PRIMARY KEY(scope,after)) WITHOUT ROWID",
    WANTED_SEALS,
];
const WANTED_SEALS: &str = "CREATE TABLE IF NOT EXISTS secure_wanted_seals(domain BLOB NOT NULL,seal BLOB NOT NULL,PRIMARY KEY(domain,seal))";
const KEPT: [(&str, &str); 3] = [
    ("secure_rebootstrap_rows", "secure_pulled_rows"),
    ("secure_rebootstrap_stubs", "secure_pulled_stubs"),
    ("secure_rebootstrap_pages", "secure_pulled_pages"),
];
fn kept(replayed: bool) -> [(&'static str, &'static str); 3] {
    let mut kept = KEPT;
    if replayed {
        kept[0].0 = "secure_replayed_staged";
    }
    kept
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn want_seal(&self, domain: Scope, seal: Commitment) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner
            .graph_for(domain)?
            .seal_by_id(seal)
            .map_err(credential)?;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT OR IGNORE INTO secure_wanted_seals VALUES(?,?)",
                    [encode(&domain)?, seal.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        flush_guard(inner, &self.inner).await.map(|_| ())
    }
    pub async fn wanted_seals(&self, domain: Scope) -> Result<Vec<Commitment>, Error> {
        Ok(self
            .wanted_seals_of_every_domain()
            .await?
            .into_iter()
            .filter(|(wanted, _)| *wanted == domain)
            .map(|(_, seal)| seal)
            .collect())
    }
    pub async fn wanted_seals_of_every_domain(&self) -> Result<Vec<(Scope, Commitment)>, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut wanted = Vec::new();
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT domain,seal FROM secure_wanted_seals ORDER BY rowid",
                    (),
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(domain), SqlValue::Blob(seal)] = record.as_slice() else {
                return Err(Error::Corrupt("wanted seal".into()));
            };
            wanted.push((decode::<Scope>(domain)?, row_of(seal)?));
        }
        let mut held = Vec::new();
        for (domain, seal) in &wanted {
            if inner.graph_for(*domain).is_err() || inner.held_seal(*domain, *seal).await?.is_some()
            {
                held.push((*domain, *seal));
            }
        }
        if !held.is_empty() {
            transaction!(inner, |inner| {
                for (domain, seal) in &held {
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_wanted_seals WHERE domain=? AND seal=?",
                            [encode(domain)?, seal.to_vec()],
                        )
                        .await?;
                }
                Ok(())
            })?;
            drop(flush_guard(inner, &self.inner).await?);
        }
        wanted.retain(|wanted| !held.contains(wanted));
        Ok(wanted)
    }
    pub async fn seal_pull_scopes(
        &self,
        domain: Scope,
        seal: Commitment,
    ) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut scopes = Vec::new();
        for (scope, id) in inner.domain_scopes(domain).await? {
            if inner.base_seal(id).await? != Some(seal)
                && inner.available_pulled_seal(id).await? != Some(seal)
            {
                scopes.push(scope);
            }
        }
        Ok(scopes)
    }
    pub async fn begin_seal_pull(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        signed.verify_authority(&inner, now()?)?;
        let statement = &signed.statement;
        let scope = statement.scope;
        let seal = statement
            .seal
            .ok_or_else(|| denied("a pulled checkpoint sits at a seal"))?;
        inner
            .graph_for(scope.domain)?
            .seal_by_id(seal)
            .map_err(credential)?;
        let generation = inner.generation_of(inner.manifest_of_tip(statement.tip)?, true)?;
        if statement.schema != generation.schema
            || signed.signer() == inner.config.node.public_key()
            || (!statement.no_frames() && Some(inner.scope_key(scope)?.0) != statement.key_epoch())
        {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        if statement.no_frames() {
            inner.check_no_frames(&signed).await?;
        }
        let complete = i64::from(statement.no_frames());
        let bytes = signed.to_bytes()?;
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            if inner
                .staging(id)
                .await?
                .is_some_and(|staging| staging.direct)
            {
                inner.abort_direct(scope, id).await?;
            }
            inner.clear_staging(id).await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,?,?,0)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(bytes.clone()),
                        SqlValue::Integer(complete),
                        SqlValue::Blob(Sets::new().to_bytes()),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        inner.load_direct().await
    }
    pub async fn finish_seal_pull(&self, scope: ResourceScope) -> Result<Commitment, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no seal pull in progress"))?;
        let staging = inner
            .staging(id)
            .await?
            .filter(|staging| !staging.direct)
            .ok_or_else(|| denied("no seal pull in progress"))?;
        let statement = staging.signed.statement.clone();
        let seal = statement
            .seal
            .ok_or_else(|| denied("no seal pull in progress"))?;
        if !staging.complete {
            return Err(denied("checkpoint rows are incomplete"));
        }
        let matches = inner.rebootstrap_moves(id).await? == statement.moves
            && staging.sets.rows_summary() == statement.rows
            && staging.sets.tombstones == statement.tombstones
            && staging.sets.creations_summary() == statement.creations;
        let refused = match matches {
            true => match inner.verify_pulled_statement(&staging.signed).await {
                Ok(()) => None,
                Err(error) if refusal(&error) => Some(error),
                Err(error) => return Err(error),
            },
            false => None,
        };
        let verified = matches && refused.is_none();
        let signed = staging.signed.to_bytes()?;
        let sets = staging.sets.to_bytes();
        transaction!(inner, |inner| {
            if verified {
                inner.drop_pulled(id).await?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_pulled VALUES(?,?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(seal.to_vec()),
                            SqlValue::Blob(signed.clone()),
                            SqlValue::Blob(sets.clone()),
                        ]),
                    )
                    .await?;
                for (staged, kept) in kept(inner.replayed_scope(scope)) {
                    inner
                        .conn
                        .execute(
                            format!("INSERT INTO {kept} SELECT * FROM {staged} WHERE scope=?"),
                            [id],
                        )
                        .await?;
                }
            }
            inner.clear_staging(id).await
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        if let Some(error) = refused {
            return Err(error);
        }
        if !verified {
            return Err(Error::Corrupt(
                "checkpoint rows differ from the signed checkpoint".into(),
            ));
        }
        Ok(seal)
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn pulled_seal(
        &self,
        id: i64,
    ) -> Result<Option<Commitment>, Error> {
        match collect_rows(
            self.conn
                .query("SELECT seal FROM secure_pulled WHERE scope=?", [id])
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Blob(seal)]) => Ok(Some(row_of(seal)?)),
            None => Ok(None),
            _ => Err(Error::Corrupt("pulled seal".into())),
        }
    }
    pub(in crate::replica) async fn drop_pulled(&self, id: i64) -> Result<(), Error> {
        for table in std::iter::once("secure_pulled").chain(KEPT.iter().map(|(_, kept)| *kept)) {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        Ok(())
    }
    pub(in crate::replica) async fn pulled_record(
        &self,
        id: i64,
    ) -> Result<Option<PulledRecord>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT s.scope,p.seal,p.signed,p.sets FROM secure_pulled p LEFT JOIN secure_scope_state s ON s.id=p.scope WHERE p.scope=?",
                    [id],
                )
                .await?,
        )
        .await?;
        let Some(record) = records.first() else {
            return Ok(None);
        };
        let corrupt = |reason: &str| Error::Corrupt(format!("pulled checkpoint {reason}"));
        let [
            SqlValue::Blob(scope),
            SqlValue::Blob(seal),
            SqlValue::Blob(signed),
            SqlValue::Blob(sets),
        ] = record.as_slice()
        else {
            return Err(corrupt("record"));
        };
        let scope: ResourceScope = decode(scope).map_err(|_| corrupt("scope"))?;
        let signed = SignedCheckpoint::from_bytes(signed).map_err(|_| corrupt("statement"))?;
        let sets = Sets::from_bytes(sets).map_err(|_| corrupt("sets"))?;
        let seal = row_of(seal).map_err(|_| corrupt("seal"))?;
        if signed.statement.scope != scope {
            return Err(corrupt("names another scope"));
        }
        if signed.statement.seal != Some(seal) {
            return Err(corrupt("names another seal"));
        }
        signed
            .verify_signature()
            .map_err(|_| corrupt("is not canonical or signed"))?;
        if signed.statement.no_frames()
            && (sets.to_bytes() != Sets::new().to_bytes()
                || self
                    .stores_anything(&KEPT.map(|(_, kept)| kept), id)
                    .await?)
        {
            return Err(corrupt("is not stored as complete empty state"));
        }
        Ok(Some(PulledRecord { signed, sets }))
    }
    async fn verify_pulled_statement(&self, signed: &SignedCheckpoint) -> Result<(), Error> {
        self.verify_import_statement(signed).await?;
        let statement = &signed.statement;
        let sealed = statement
            .seal
            .ok_or_else(|| denied("a pulled checkpoint sits at a seal"))?;
        let graph = self.graph_for(statement.scope.domain)?;
        if graph.revoked.contains(&sealed) {
            return Err(denied("a pulled checkpoint's seal was revoked"));
        }
        let seal = graph.seal_by_id(sealed).map_err(credential)?;
        if seal.statement.scope != statement.scope.domain || seal.statement.tip != statement.tip {
            return Err(denied("a pulled checkpoint names its seal's tip"));
        }
        Ok(())
    }
    pub(in crate::replica) async fn available_pulled_seal(
        &self,
        id: i64,
    ) -> Result<Option<Commitment>, Error> {
        let Some(pulled) = self.pulled_record(id).await? else {
            return Ok(None);
        };
        match self.verify_pulled_statement(&pulled.signed).await {
            Ok(()) => Ok(pulled.signed.statement.seal),
            Err(error) if refusal(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub(in crate::replica) async fn stale_pulled(&self, id: i64) -> Result<bool, Error> {
        let Some(pulled) = self.pulled_record(id).await? else {
            return Ok(false);
        };
        match self.verify_pulled_statement(&pulled.signed).await {
            Ok(()) => Ok(self.base_seal(id).await? == pulled.signed.statement.seal),
            Err(error) if refusal(&error) => Ok(true),
            Err(error) => Err(error),
        }
    }
    pub(in crate::replica) async fn revalidate_pulled(&self) -> Result<(), Error> {
        for record in rows(&self.conn, "SELECT scope FROM secure_pulled").await? {
            let [SqlValue::Integer(id)] = record.as_slice() else {
                return Err(Error::Corrupt("pulled checkpoint".into()));
            };
            let Some(pulled) = self.pulled_record(*id).await? else {
                continue;
            };
            match self.verify_pulled_statement(&pulled.signed).await {
                Ok(()) => {}
                Err(error) if refusal(&error) => self.drop_pulled(*id).await?,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    pub(in crate::replica) async fn install_pulled(
        &mut self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<PulledInstall, Error> {
        let PulledRecord { signed, sets } = self
            .pulled_record(id)
            .await?
            .ok_or_else(|| denied("no state was pulled for this scope"))?;
        match self.verify_pulled_statement(&signed).await {
            Ok(()) => {}
            Err(error) if refusal(&error) => {
                self.drop_pulled(id).await?;
                return Ok(PulledInstall::Refused);
            }
            Err(error) => return Err(error),
        }
        if self.staging(id).await?.is_some() {
            return Ok(PulledInstall::Deferred);
        }
        for (staged, kept) in kept(self.replayed_scope(scope)) {
            self.conn
                .execute(
                    format!("INSERT INTO {staged} SELECT * FROM {kept} WHERE scope=?"),
                    [id],
                )
                .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,1,?,0)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Blob(signed.to_bytes()?),
                    SqlValue::Blob(sets.to_bytes()),
                ]),
            )
            .await?;
        self.switch_to_staged(scope, id, &signed, &sets, false, now()?)
            .await?;
        self.drop_pulled(id).await?;
        self.verified_at.remove(&id);
        Ok(PulledInstall::Installed)
    }
}

pub(in crate::replica) struct PulledRecord {
    signed: SignedCheckpoint,
    sets: Sets,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::replica) enum PulledInstall {
    Installed,
    Refused,
    Deferred,
}
fn refusal(error: &Error) -> bool {
    matches!(
        error,
        Error::Rejected(_) | Error::Protocol(_) | Error::Incompatible(_)
    )
}
