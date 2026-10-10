use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DurabilityPolicy {
    AsyncReplication,
    RequiredCustodians {
        nodes: BTreeSet<PublicKey>,
        minimum: usize,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustodyAck {
    pub package: Commitment,
    pub closure: Commitment,
    pub covered_results: Vec<Commitment>,
    node: PublicKey,
    credentials: CredentialChain,
    scopes: Vec<ResourceScope>,
    signature: Signature,
}
impl CustodyAck {
    fn signing_bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.recovery-custody.v1",
            &(
                self.package,
                self.closure,
                &self.covered_results,
                self.node,
                &self.credentials,
                &self.scopes,
            ),
        )
    }
}
impl DurabilityPolicy {
    pub(crate) async fn verify<A: RowCommandApplication>(
        &self,
        replica: &EncryptedReplica<A>,
        result: &ConfirmedCommand,
        acks: &[CustodyAck],
    ) -> Result<(), Error> {
        let Self::RequiredCustodians { nodes, minimum } = self else {
            return Ok(());
        };
        if *minimum == 0 || *minimum > nodes.len() {
            return Err(denied("invalid custody threshold"));
        }
        let inner = replica.inner.lock().await;
        let package = result.strong_package()?;
        let scopes = resources(&inner, &package.request)?;
        let mut seen = BTreeSet::new();
        for ack in acks {
            if ack.node == inner.config.node.public_key()
                || !nodes.contains(&ack.node)
                || !ack
                    .covered_results
                    .contains(&package.confirmation()?.new_head)
                || !scopes.iter().all(|scope| ack.scopes.contains(scope))
                || !ack.node.verify(&ack.signing_bytes()?, &ack.signature)
            {
                return Err(denied("invalid recovery custody acknowledgment"));
            }
            let grants = inner.verify_chain(&ack.credentials, ack.node, now()?)?;
            authorize(&grants, &scopes)?;
            seen.insert(ack.node);
        }
        if seen.len() < *minimum {
            return Err(denied("required recovery custody unavailable"));
        }
        Ok(())
    }
}
impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Acknowledges durable encrypted custody and verified dependency closure, not application.
    pub async fn retain_recovery(
        &self,
        package: &RecoveryPackage,
        key: &[u8; 32],
    ) -> Result<CustodyAck, Error> {
        let payload = package.open(key)?;
        let mut inner = self.inner.lock().await;
        package.inspect(inner.config.publisher, &inner.config.revocations)?;
        if package.inventory.recipient != inner.config.node.public_key()
            || package.inventory.definition != inner.manifest
        {
            return Err(denied("custody recipient mismatch"));
        }
        authorize(&inner.local_grants(now()?)?, &package.inventory.scopes)?;
        for frame in &payload.frames {
            inner.verify_frame(frame, now()?)?;
        }
        let mut covered_results = Vec::new();
        for result in payload
            .results
            .iter()
            .chain(payload.preparations.iter().map(|(_, result)| result))
        {
            let strong = result.strong_package()?;
            let binding = PackageSet::from_bytes(&strong.rows)?;
            let actual: BTreeSet<_> = result
                .parts
                .iter()
                .map(EncryptedPackage::commitment)
                .collect::<Result<_, _>>()?;
            if binding.commitments.iter().any(|id| !actual.contains(id)) {
                return Err(denied("custody result closure incomplete"));
            }
            // Preparations reserve no receipt yet; verify with its prospective receipt.
            let mut candidate = result.clone();
            candidate.receipt = rocky_core::StrongReceipt {
                confirmation: strong.confirmation()?,
                sequence: strong.sequence,
            }
            .encode();
            candidate.verify_history(&inner)?;
            covered_results.push(strong.confirmation()?.new_head);
        }
        covered_results.sort();
        covered_results.dedup();
        let package_id = package.commitment()?;
        let bytes = package.to_bytes()?;
        transaction!(inner, |inner| {
            let total = rows(
                &inner.conn,
                "SELECT COALESCE(SUM(LENGTH(package)),0) FROM secure_recovery_custody",
            )
            .await?;
            let Some([SqlValue::Integer(size)]) = total.first().map(Vec::as_slice) else {
                return Err(denied("corrupt custody budget"));
            };
            if *size as usize + bytes.len() > MAX_SECURE_BYTES {
                return Err(denied("custody storage quota"));
            }
            inner
                .conn
                .execute(
                    "INSERT INTO secure_recovery_custody VALUES(?,?) ON CONFLICT(id) DO NOTHING",
                    [package_id.to_vec(), bytes.clone()],
                )
                .await?;
            Ok(())
        })?;
        // The sender may rely on this copy once it holds the acknowledgment.
        inner.flush()?;
        let mut ack = CustodyAck {
            package: package_id,
            closure: package.inventory.closure,
            covered_results,
            node: inner.config.node.public_key(),
            credentials: inner.config.credentials.clone(),
            scopes: package.inventory.scopes.clone(),
            signature: inner.config.node.sign(b""),
        };
        ack.signature = inner.config.node.sign(&ack.signing_bytes()?);
        Ok(ack)
    }
    pub async fn retained_recovery(
        &self,
        id: Commitment,
    ) -> Result<Option<RecoveryPackage>, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        let mut query = inner
            .conn
            .query(
                "SELECT package FROM secure_recovery_custody WHERE id=?",
                [id.to_vec()],
            )
            .await?;
        query
            .next()
            .await?
            .map(|row| RecoveryPackage::from_bytes(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    pub async fn set_recovery_durability(
        &self,
        scope: Scope,
        policy: DurabilityPolicy,
    ) -> Result<(), Error> {
        if let DurabilityPolicy::RequiredCustodians { nodes, minimum } = &policy
            && (*minimum == 0 || *minimum > nodes.len() || nodes.len() > 32)
        {
            return Err(denied("invalid custody threshold"));
        }
        let mut inner = self.inner.lock().await;
        if !inner.local_grants(now()?)?.iter().any(|grant| {
            grant.scope.domain == scope && grant.rights.contains(&rocky_core::Right::Execute)
        }) {
            return Err(denied("durability configuration denied"));
        }
        let values = [encode(&scope)?, encode(&policy)?];
        transaction!(inner, |inner| {
            inner.conn.execute("INSERT INTO secure_durability VALUES(?,?) ON CONFLICT(domain) DO UPDATE SET policy=excluded.policy", values.clone()).await?;
            Ok(())
        })
    }
    pub(crate) async fn recovery_durability(
        &self,
        scope: Scope,
    ) -> Result<DurabilityPolicy, Error> {
        let inner = self.inner.lock().await;
        let mut records = inner
            .conn
            .query(
                "SELECT policy FROM secure_durability WHERE domain=?",
                [encode(&scope)?],
            )
            .await?;
        match records.next().await? {
            Some(row) => decode(&row.get::<Vec<u8>>(0)?),
            None => Ok(DurabilityPolicy::AsyncReplication),
        }
    }
}
