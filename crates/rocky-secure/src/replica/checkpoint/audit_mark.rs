use super::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_audit_marks(scope INTEGER PRIMARY KEY,position INTEGER NOT NULL,mark BLOB NOT NULL,signature BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_audit_before(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TRIGGER IF NOT EXISTS secure_audit_before_insert BEFORE INSERT ON secure_rows WHEN EXISTS(SELECT 1 FROM secure_audit_marks WHERE scope=NEW.scope) BEGIN INSERT INTO secure_audit_before SELECT NEW.scope,NEW.row,coalesce((SELECT fields FROM secure_rows WHERE scope=NEW.scope AND row=NEW.row),X'') WHERE NOT EXISTS(SELECT 1 FROM secure_audit_before WHERE scope=NEW.scope AND row=NEW.row); END",
    "CREATE TRIGGER IF NOT EXISTS secure_audit_before_update BEFORE UPDATE OF fields ON secure_rows WHEN EXISTS(SELECT 1 FROM secure_audit_marks WHERE scope=OLD.scope) BEGIN INSERT INTO secure_audit_before SELECT OLD.scope,OLD.row,OLD.fields WHERE NOT EXISTS(SELECT 1 FROM secure_audit_before WHERE scope=OLD.scope AND row=OLD.row); END",
    "CREATE TRIGGER IF NOT EXISTS secure_audit_before_delete BEFORE DELETE ON secure_rows WHEN EXISTS(SELECT 1 FROM secure_audit_marks WHERE scope=OLD.scope) BEGIN INSERT INTO secure_audit_before SELECT OLD.scope,OLD.row,OLD.fields WHERE NOT EXISTS(SELECT 1 FROM secure_audit_before WHERE scope=OLD.scope AND row=OLD.row); END",
    "CREATE TRIGGER IF NOT EXISTS secure_audit_status AFTER UPDATE OF status ON secure_operations WHEN OLD.status IS NOT NEW.status BEGIN DELETE FROM secure_audit_marks WHERE EXISTS(SELECT 1 FROM secure_applied a WHERE a.hash=NEW.hash AND a.scope=secure_audit_marks.scope AND a.position<=secure_audit_marks.position); END",
    "CREATE TRIGGER IF NOT EXISTS secure_audit_scope_delete AFTER DELETE ON secure_scope_state BEGIN DELETE FROM secure_audit_marks WHERE scope=OLD.id; DELETE FROM secure_audit_before WHERE scope=OLD.id; END",
];

pub(super) const AUDITED_ROW: &str = "SELECT fields FROM (SELECT coalesce((SELECT fields FROM secure_audit_before WHERE scope=?1 AND row=?2),(SELECT fields FROM secure_rows WHERE scope=?1 AND row=?2)) AS fields) WHERE fields IS NOT NULL";
const AUDIT_MARK_LABEL: &str = "rocky.audit-mark.v1";
const AUDIT_MARK_KEY_LABEL: &str = "rocky.audit-mark-key.v1";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Basis {
    scope: ResourceScope,
    checkpoint: Option<(i64, Option<Commitment>)>,
    tip: TipId,
    keys: BTreeSet<(rocky_core::KeyEpoch, Commitment)>,
}

#[derive(Serialize, Deserialize)]
struct Statement {
    basis: Basis,
    position: i64,
    applied: i64,
    tail: Option<(serde_bytes::ByteBuf, Vec<IdentityMark>)>,
    ledger: Option<Vec<IdentityMark>>,
}

#[derive(Clone)]
pub(in crate::replica) struct Tail {
    pub(in crate::replica) sets: Sets,
    pub(in crate::replica) replayed_ledger: identity::IdentityLedger,
}

pub(in crate::replica) struct AuditMark {
    pub(in crate::replica) position: i64,
    pub(in crate::replica) tail: Option<Tail>,
    pub(in crate::replica) ledger: Option<identity::IdentityLedger>,
}

#[derive(Default)]
pub(in crate::replica) struct Audited {
    pub(in crate::replica) tail: Option<Tail>,
    pub(in crate::replica) ledger: Option<identity::IdentityLedger>,
}

impl<A: RowApplication> Inner<A> {
    async fn audit_basis(&self, scope: ResourceScope, id: i64) -> Result<Option<Basis>, Error> {
        let checkpoint = match self.checkpoint_record(id, true).await? {
            Some(record) => Some((
                record.position,
                record.signed.map(|signed| signed.id()).transpose()?,
            )),
            None if self
                .staging(id)
                .await?
                .is_some_and(|staging| staging.direct) =>
            {
                return Ok(None);
            }
            None => None,
        };
        Ok(Some(Basis {
            scope,
            checkpoint,
            tip: self.workspace.tip,
            keys: self
                .opening_keys()
                .filter(|key| key.scope == scope)
                .map(|key| {
                    (
                        key.generation,
                        rocky_core::commitment(AUDIT_MARK_KEY_LABEL, &key.key),
                    )
                })
                .collect(),
        }))
    }
    async fn applied_through(&self, id: i64, through: i64) -> Result<(i64, i64), Error> {
        match collect_rows(
            self.conn
                .query(
                    "SELECT COALESCE(MAX(position),0),COUNT(*) FROM secure_applied WHERE scope=? AND position<=?",
                    [id, through],
                )
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(head), SqlValue::Integer(count)]) => Ok((*head, *count)),
            _ => Err(Error::Corrupt("applied positions".into())),
        }
    }
    fn audit_mark_bytes(&self, id: i64, statement: &[u8]) -> Result<Vec<u8>, Error> {
        encode(&(AUDIT_MARK_LABEL, id, serde_bytes::Bytes::new(statement)))
    }
    pub(in crate::replica) async fn audit_mark(
        &self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<Option<AuditMark>, Error> {
        let stored = collect_rows(
            self.conn
                .query(
                    "SELECT mark,signature FROM secure_audit_marks WHERE scope=?",
                    [id],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes), SqlValue::Blob(signature)]) =
            stored.first().map(Vec::as_slice)
        else {
            return Ok(None);
        };
        let Ok(signature) = Signature::try_from(signature.as_slice()) else {
            return Ok(None);
        };
        if !self
            .config
            .node
            .public_key()
            .verify(&self.audit_mark_bytes(id, bytes)?, &signature)
        {
            return Ok(None);
        }
        let Ok(statement) = decode::<Statement>(bytes) else {
            return Ok(None);
        };
        if Some(&statement.basis) != self.audit_basis(scope, id).await?.as_ref()
            || self.applied_through(id, statement.position).await?.1 != statement.applied
        {
            return Ok(None);
        }
        Ok(Some(AuditMark {
            position: statement.position,
            tail: statement
                .tail
                .map(|(sets, replayed_ledger)| {
                    Ok::<_, Error>(Tail {
                        sets: Sets::from_bytes(&sets)?,
                        replayed_ledger: identity::IdentityLedger::seeded(&replayed_ledger)?,
                    })
                })
                .transpose()?,
            ledger: statement
                .ledger
                .map(|marks| identity::IdentityLedger::seeded(&marks))
                .transpose()?,
        }))
    }
    pub(in crate::replica) async fn store_audit_mark(
        &self,
        scope: ResourceScope,
        id: i64,
        audited: Audited,
    ) -> Result<(), Error> {
        let Some(basis) = self
            .audit_basis(scope, id)
            .await?
            .filter(|_| audited.tail.is_some() || audited.ledger.is_some())
        else {
            return self.forget_audit_mark(id).await;
        };
        let (position, applied) = self.applied_through(id, i64::MAX).await?;
        let statement = encode(&Statement {
            basis,
            position,
            applied,
            tail: audited.tail.map(|tail| {
                (
                    serde_bytes::ByteBuf::from(tail.sets.to_bytes()),
                    tail.replayed_ledger.into_marks(),
                )
            }),
            ledger: audited.ledger.map(identity::IdentityLedger::into_marks),
        })?;
        let signature = self
            .config
            .node
            .sign(&self.audit_mark_bytes(id, &statement)?);
        self.forget_audit_mark(id).await?;
        self.conn
            .execute(
                "INSERT INTO secure_audit_marks VALUES(?,?,?,?)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(position),
                    SqlValue::Blob(statement),
                    SqlValue::Blob(signature.to_bytes().to_vec()),
                ]),
            )
            .await?;
        Ok(())
    }
    async fn forget_audit_mark(&self, id: i64) -> Result<(), Error> {
        self.conn
            .execute("DELETE FROM secure_audit_marks WHERE scope=?", [id])
            .await?;
        self.conn
            .execute("DELETE FROM secure_audit_before WHERE scope=?", [id])
            .await?;
        Ok(())
    }
}
