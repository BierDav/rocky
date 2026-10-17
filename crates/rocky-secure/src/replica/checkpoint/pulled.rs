//! Seal states pulled from a node that keeps them. A verified checkpoint at
//! a seal this replica does not hold is kept aside; following a tip whose
//! path passes the seal installs it as the scope's replay base. See
//! `docs/runtime/kept-seals.md`.
use super::*;
use rocky_core::Scope;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_pulled(scope INTEGER PRIMARY KEY,seal BLOB NOT NULL,signed BLOB NOT NULL,sets BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_pulled_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row))",
    "CREATE TABLE IF NOT EXISTS secure_pulled_stubs(scope INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_pulled_pages(scope INTEGER NOT NULL,after BLOB NOT NULL,next BLOB,signature BLOB NOT NULL,PRIMARY KEY(scope,after)) WITHOUT ROWID",
    // Seals whose state the next sessions pull from peers.
    "CREATE TABLE IF NOT EXISTS secure_wanted_seals(domain BLOB NOT NULL,seal BLOB NOT NULL,PRIMARY KEY(domain,seal))",
];
/// The staging tables and the pulled tables that keep their contents.
const KEPT: [(&str, &str); 3] = [
    ("secure_rebootstrap_rows", "secure_pulled_rows"),
    ("secure_rebootstrap_stubs", "secure_pulled_stubs"),
    ("secure_rebootstrap_pages", "secure_pulled_pages"),
];

impl<A: RowApplication> EncryptedReplica<A> {
    /// Asks the next sync sessions to pull `seal`'s state from peers that
    /// keep it, until this node holds it.
    pub async fn want_seal(&self, domain: Scope, seal: Commitment) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
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
        inner.flush()
    }
    /// The seals still to pull, oldest request first; those now held are
    /// no longer wanted.
    pub async fn wanted_seals(&self, domain: Scope) -> Result<Vec<Commitment>, Error> {
        let mut inner = self.inner.lock().await;
        let mut wanted = Vec::new();
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT seal FROM secure_wanted_seals WHERE domain=? ORDER BY rowid",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(seal)] = record.as_slice() else {
                return Err(Error::Corrupt("wanted seal".into()));
            };
            wanted.push(row_of(seal)?);
        }
        let mut held = Vec::new();
        for seal in &wanted {
            if inner.graph_for(domain).is_err() || inner.held_seal(domain, *seal).await?.is_some() {
                held.push(*seal);
            }
        }
        if !held.is_empty() {
            transaction!(inner, |inner| {
                for seal in &held {
                    inner
                        .conn
                        .execute(
                            "DELETE FROM secure_wanted_seals WHERE domain=? AND seal=?",
                            [encode(&domain)?, seal.to_vec()],
                        )
                        .await?;
                }
                Ok(())
            })?;
            inner.flush()?;
        }
        wanted.retain(|seal| !held.contains(seal));
        Ok(wanted)
    }
    /// The held scopes of `domain` whose state at `seal` a pull still
    /// needs: neither their replay base nor a pulled state sits there.
    pub async fn seal_pull_scopes(
        &self,
        domain: Scope,
        seal: Commitment,
    ) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock().await;
        let mut scopes = Vec::new();
        for (scope, id) in inner.domain_scopes(domain).await? {
            if inner.base_seal(id).await? != Some(seal)
                && inner.pulled_seal(id).await? != Some(seal)
            {
                scopes.push(scope);
            }
        }
        Ok(scopes)
    }
    /// Starts pulling `signed`, a peer's checkpoint at a seal, to hold that
    /// seal's state: its pages stage as for an import, and
    /// `finish_seal_pull` keeps them aside. The scope's rows do not change.
    pub async fn begin_seal_pull(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        signed.verify_authority(&inner, now()?)?;
        inner.check_binding(&signed.statement)?;
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
            || inner.scope_key(scope)?.0 != statement.key_epoch
        {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
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
                    "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,0,?,0)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(bytes.clone()),
                        SqlValue::Blob(Sets::new().to_bytes()),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        inner.load_direct().await
    }
    /// Verifies the staged pages against the pulled checkpoint and keeps
    /// them aside as this replica's state at its seal, which it returns.
    pub async fn finish_seal_pull(&self, scope: ResourceScope) -> Result<Commitment, Error> {
        let mut inner = self.inner.lock().await;
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
        let verified = inner.rebootstrap_moves(id).await? == statement.moves
            && staging.sets.rows_summary() == statement.rows
            && staging.sets.tombstones == statement.tombstones
            && staging.sets.creations_summary() == statement.creations
            && inner.scope_key(scope)?.0 == statement.key_epoch;
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
                for (staged, kept) in KEPT {
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
        inner.flush()?;
        if !verified {
            return Err(Error::Corrupt(
                "checkpoint rows differ from the signed checkpoint".into(),
            ));
        }
        Ok(seal)
    }
}

impl<A: RowApplication> Inner<A> {
    /// The seal of the state pulled for scope `id`, if any.
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
    async fn drop_pulled(&self, id: i64) -> Result<(), Error> {
        for table in std::iter::once("secure_pulled").chain(KEPT.iter().map(|(_, kept)| *kept)) {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        Ok(())
    }
    /// Inside the caller's transaction, makes the state pulled for `scope`
    /// its replay base, as a finished import does; its operations apply
    /// again on top.
    pub(in crate::replica) async fn install_pulled(
        &mut self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<(), Error> {
        let records = collect_rows(
            self.conn
                .query("SELECT signed,sets FROM secure_pulled WHERE scope=?", [id])
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(signed), SqlValue::Blob(sets)]) =
            records.first().map(Vec::as_slice)
        else {
            return Err(denied("no state was pulled for this scope"));
        };
        let signed = SignedCheckpoint::from_bytes(signed)?;
        let sets = Sets::from_bytes(sets)?;
        signed.verify_authority(self, now()?)?;
        self.clear_staging(id).await?;
        for (staged, kept) in KEPT {
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
        Ok(())
    }
}
