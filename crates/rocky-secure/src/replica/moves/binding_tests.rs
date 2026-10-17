use super::*;

impl CreationProof {
    pub(crate) fn damage_signature_for_test(&mut self, signer: &PrivateKey) {
        self.signature = signer.sign(b"wrong creation message");
    }
    pub(crate) fn lower_stamp_for_test(&mut self, signer: &PrivateKey) {
        self.stamp.clock -= 1;
        self.signature = signer.sign(&self.bytes().unwrap());
    }
    pub(crate) fn check_binding_differentials_for_test(
        &self,
        node: &PrivateKey,
        publisher: PublicKey,
    ) {
        let ceiling = self.stamp.clone();
        let render = |result: Result<(), Error>| {
            result.map_err(|error| match error {
                Error::Rejected(reason) => reason,
                error => error.to_string(),
            })
        };
        let other = PrivateKey::from_bytes(&[222; 32]);
        let mut cases = Vec::new();
        // Each changed request reaches the chain gate with a valid own signature.
        cases.push((
            self.clone(),
            other.public_key(),
            self.table,
            self.id,
            ceiling.clone(),
        ));
        let mut proof = self.clone();
        proof.table = [223; 32];
        proof.signature = node.sign(&proof.bytes().unwrap());
        cases.push((proof, publisher, [223; 32], self.id, ceiling.clone()));
        let mut proof = self.clone();
        proof.id = rocky_core::RowIdentity::next(self.id.author() + 1, 0).unwrap();
        proof.signature = node.sign(&proof.bytes().unwrap());
        let id = proof.id;
        cases.push((proof, publisher, self.table, id, ceiling.clone()));
        let mut proof = self.clone();
        proof.stamp.author = other.public_key().into();
        proof.signature = other.sign(&proof.bytes().unwrap());
        let changed_ceiling = proof.stamp.clone();
        cases.push((proof, publisher, self.table, self.id, changed_ceiling));
        for field in ["version", "authors", "signature"] {
            let mut proof = self.clone();
            let mut binding = serde_json::to_value(&proof.bindings[0]).unwrap();
            binding[field] = match field {
                "version" => serde_json::json!(0),
                "authors" => serde_json::Value::Null,
                _ => serde_json::to_value(other.sign(b"wrong binding message")).unwrap(),
            };
            proof.bindings[0] = serde_json::from_value(binding).unwrap();
            proof.signature = node.sign(&proof.bytes().unwrap());
            cases.push((proof, publisher, self.table, self.id, ceiling.clone()));
        }
        for count in [0, rocky_credentials::MAX_CREDENTIAL_DEPTH + 1] {
            let mut proof = self.clone();
            proof.bindings = vec![proof.bindings[0].clone(); count];
            proof.signature = node.sign(&proof.bytes().unwrap());
            cases.push((proof, publisher, self.table, self.id, ceiling.clone()));
        }
        let mut invalid = Vec::new();
        for field in ["version", "manifest", "table", "id", "ceiling", "signature"] {
            let mut proof = self.clone();
            let mut ceiling = ceiling.clone();
            match field {
                "version" => proof.version = 0,
                "manifest" => proof.manifest = [224; 32],
                "table" => proof.table = [225; 32],
                "id" => {
                    proof.id =
                        rocky_core::RowIdentity::next(self.id.author(), self.id.sequence()).unwrap()
                }
                "ceiling" => ceiling.clock -= 1,
                _ => proof.damage_signature_for_test(node),
            }
            invalid.push((proof, ceiling));
        }
        let mut early = self.clone();
        early.version = 0;
        early.stamp.author = Author(
            (0..=255)
                .map(|byte| [byte; 32])
                .find(|bytes| PublicKey::from_bytes(bytes).is_err())
                .unwrap(),
        );
        assert!(matches!(early.stamp.author.key(), Err(Error::Corrupt(_))));
        invalid.push((early, ceiling.clone()));
        let mut lower = self.clone();
        lower.lower_stamp_for_test(node);
        assert!(lower.stamp < ceiling);
        let cloned = self.clone();
        let mut memo = FrameBindings::default();
        self.verify_with_bindings(
            publisher,
            self.manifest,
            self.table,
            self.id,
            &ceiling,
            Some(&mut memo),
        )
        .unwrap();
        cloned
            .verify_with_bindings(
                publisher,
                self.manifest,
                self.table,
                self.id,
                &ceiling,
                Some(&mut memo),
            )
            .unwrap();
        assert_eq!((memo.own_gates, memo.full, memo.hits), (2, 1, 1));
        for (proof, publisher, table, id, ceiling) in &cases {
            let hits = memo.hits;
            let full = memo.full;
            let old = render(proof.verify(*publisher, proof.manifest, *table, *id, ceiling));
            assert!(old.is_err());
            assert_eq!(
                render(proof.verify_with_bindings(
                    *publisher,
                    proof.manifest,
                    *table,
                    *id,
                    ceiling,
                    Some(&mut memo)
                )),
                old
            );
            assert_eq!(memo.hits, hits);
            assert_eq!(memo.full, full + 1);
        }
        // Failures leave the original successful slot intact.
        let hits = memo.hits;
        self.verify_with_bindings(
            publisher,
            self.manifest,
            self.table,
            self.id,
            &ceiling,
            Some(&mut memo),
        )
        .unwrap();
        assert_eq!(memo.hits, hits + 1);
        for (proof, ceiling) in &invalid {
            let old = render(proof.verify(publisher, self.manifest, self.table, self.id, ceiling));
            assert_eq!(old, Err("invalid signed row creation proof".into()));
            let counters = (memo.full, memo.hits);
            assert_eq!(
                render(proof.verify_with_bindings(
                    publisher,
                    self.manifest,
                    self.table,
                    self.id,
                    ceiling,
                    Some(&mut memo)
                )),
                old
            );
            assert_eq!((memo.full, memo.hits), counters);
        }
        let old = render(lower.verify(publisher, self.manifest, self.table, self.id, &ceiling));
        assert_eq!(old, Ok(()));
        assert_eq!(
            render(lower.verify_with_bindings(
                publisher,
                self.manifest,
                self.table,
                self.id,
                &ceiling,
                Some(&mut memo)
            )),
            old
        );
        drop(memo);
    }
}
