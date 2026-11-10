use super::*;
use crate::{AuthorRange, Authority, CredentialChain, PrivateKey, RevocationSet};
use rocky_core::{
    DomainId, MeshId, PartitionSelection, PartitionTuple, PartitionValue, PrincipalKind, Right,
    Scope,
};
use serde_json::json;
use std::collections::BTreeSet;

const TIME: u64 = 10_000;
fn text(value: &str) -> PartitionValue {
    PartitionValue::new(
        rocky_core::KeyColumn {
            affinity: rocky_core::Affinity::Text,
            collation: rocky_core::Collation::Binary,
        },
        rocky_core::SqlValue::Text(value.into()),
    )
    .unwrap()
}
struct Fixture {
    root: PrivateKey,
    identity: PrivateKey,
    node: PrivateKey,
    families: GrantFamilies,
    resources: Vec<ResourceGrant>,
    partitions: Vec<PartitionGrant>,
}
impl Fixture {
    fn new() -> Self {
        let root = PrivateKey::from_bytes(&[1; 32]);
        let publisher: String = root
            .public_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let tables: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|name| {
                let slot = if name == "b" { "tenant" } else { "book" };
                let mut table = json!({"name":name,"class":"replayed","columns":[
                {"name":slot,"type":"text","not_null":true},{"name":"title","type":"text"}],
                "primary_key":[slot,"id"],"partition_by":[slot],"partition_group":"book"});
                if name == "b" {
                    table["granted_like"] = json!("a");
                }
                table
            })
            .collect();
        let schema = rocky_core::SchemaDefinition::parse(
            json!({"format":6,"publisher":publisher,"application":"families","tables":tables})
                .to_string()
                .as_bytes(),
        )
        .unwrap()
        .build()
        .unwrap();
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([2; 16]),
        };
        let resources: Vec<_> = schema
            .tables
            .iter()
            .enumerate()
            .map(|(i, table)| ResourceGrant {
                scope: ResourceScope {
                    domain,
                    table: schema.table_id(table),
                    encryption: [i as u8; 32],
                },
                rows: RowSelection::All,
                fields: FieldSelection::All,
                rights: [
                    Right::Read,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                    Right::Custody,
                ]
                .into(),
            })
            .collect();
        let partitions = schema
            .tables
            .iter()
            .zip(&resources)
            .map(|(table, resource)| PartitionGrant {
                domain,
                table: resource.scope.table,
                selector: PartitionSelector(
                    [(
                        schema.field_id(table, table.column(&table.partition_by[0]).unwrap()),
                        PartitionSelection::Only([text("A")].into()),
                    )]
                    .into(),
                ),
                rows: RowSelection::All,
                fields: FieldSelection::All,
                rights: resource.rights.clone(),
            })
            .collect();
        let families = GrantFamilies::new(
            &schema.manifest(&[domain.domain]),
            &resources
                .iter()
                .map(|grant| grant.scope)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        Self {
            root,
            identity: PrivateKey::from_bytes(&[3; 32]),
            node: PrivateKey::from_bytes(&[4; 32]),
            families,
            resources,
            partitions,
        }
    }
    fn chain(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
    ) -> CredentialChain {
        self.chain_terms(grants, partitions, true)
    }
    fn old_chain(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
    ) -> CredentialChain {
        self.chain_terms(grants, partitions, false)
    }
    fn chain_terms(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        families: bool,
    ) -> CredentialChain {
        let authority = Authority::owner_in(
            &self.root,
            crate::DatabaseContext {
                publisher: self.root.public_key(),
                scope: self.resources[0].scope.domain,
            },
        )
        .unwrap();
        let issue = if families {
            Authority::issue_family_terms
        } else {
            Authority::issue_partition_terms
        };
        let capability = issue(
            &authority,
            self.identity.public_key(),
            grants,
            partitions,
            (3, Some(AuthorRange::new(1, 1000).unwrap())),
            (TIME, 3600, 0),
        )
        .unwrap();
        crate::device_chain(capability, &self.identity, self.node.public_key(), 1).unwrap()
    }
    fn tuple(&self, table: usize, value: &str) -> (ResourceScope, PartitionTuple) {
        let slot = *self.partitions[table].selector.0.keys().next().unwrap();
        let tuple = PartitionTuple::new(vec![(slot, text(value))]).unwrap();
        let scope = tuple
            .scope(
                self.resources[table].scope.domain,
                self.resources[table].scope.table,
                &[5; 32],
            )
            .unwrap();
        (scope, tuple)
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn compact_families_and_per_table_credentials_verify_without_changing_signed_bytes() {
    let f = Fixture::new();
    let resources = f.resources[..2].to_vec();
    let partitions = f.partitions[..2].to_vec();
    let compact = f.families.compact_resources(&resources);
    let selectors = f.families.compact_partitions(&partitions);
    assert_eq!(compact, resources[..1]);
    assert_eq!(selectors, partitions[..1]);
    assert_eq!(f.families.resources(&compact), resources);
    assert_eq!(f.families.partitions(&selectors), partitions);
    for chain in [
        f.old_chain(resources, partitions),
        f.chain(compact, selectors),
    ] {
        let bytes = crate::encode(&chain).unwrap();
        let ids: Vec<_> = chain.0.iter().map(|link| link.id().unwrap()).collect();
        let held = chain.held().unwrap();
        let renewed = Authority::owner_in(
            &f.root,
            crate::DatabaseContext {
                publisher: f.root.public_key(),
                scope: f.resources[0].scope.domain,
            },
        )
        .unwrap()
        .renew(held, &RevocationSet::new(), (TIME, 3600, 0))
        .unwrap();
        assert_eq!(
            renewed.capability.extended().unwrap().families,
            held.extended().unwrap().families,
            "renewal retains the signed choice between exact and family authority"
        );
        let verified = chain
            .verified_with_families(
                f.root.public_key(),
                &RevocationSet::new(),
                TIME,
                &f.families,
            )
            .unwrap();
        for table in 0..2 {
            let (scope, tuple) = f.tuple(table, "A");
            let grants = verified
                .partition_author(f.node.public_key(), 1, scope, &tuple, TIME)
                .unwrap();
            assert_eq!(grants[0].rights, f.resources[table].rights);
            assert_eq!(grants[0].fields, FieldSelection::All);
            assert!(
                verified
                    .partition_author(f.node.public_key(), 2, scope, &tuple, TIME)
                    .is_err()
            );
            let (scope, tuple) = f.tuple(table, "B");
            assert!(
                verified
                    .partition(f.node.public_key(), scope, &tuple, TIME)
                    .is_err()
            );
        }
        let (scope, tuple) = f.tuple(2, "A");
        assert!(
            verified
                .partition(f.node.public_key(), scope, &tuple, TIME)
                .is_err(),
            "partition group membership does not grant authority"
        );
        assert!(
            verified
                .selectors(f.node.public_key(), TIME + 3600)
                .is_err()
        );
        assert_eq!(crate::encode(&chain).unwrap(), bytes);
        assert_eq!(
            chain
                .0
                .iter()
                .map(|link| link.id().unwrap())
                .collect::<Vec<_>>(),
            ids
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn restricted_fields_rows_and_explicit_follower_rights_never_widen() {
    let f = Fixture::new();
    for fields in [FieldSelection::Only([[8; 32]].into()), FieldSelection::All] {
        let mut grant = f.partitions[0].clone();
        grant.fields = fields.clone();
        if fields == FieldSelection::All {
            grant.rows = RowSelection::Only([[7; 32]].into());
        }
        assert_eq!(f.families.partitions(&[grant.clone()]), [grant]);
    }
    let mut follower = f.partitions[1].clone();
    follower.rights = [Right::Read].into();
    let parts = vec![f.partitions[0].clone(), follower.clone()];
    assert_eq!(f.families.compact_partitions(&parts), parts);
    assert_eq!(f.families.partitions(&parts), parts);
    let mut concrete = f.resources[1].clone();
    concrete.fields = FieldSelection::Only([[8; 32]].into());
    let resources = vec![f.resources[0].clone(), concrete.clone()];
    assert_eq!(f.families.compact_resources(&resources), resources);
    assert_eq!(f.families.resources(&resources), resources);
    let chain = f.chain(resources, parts);
    let verified = chain
        .verified_with_families(
            f.root.public_key(),
            &RevocationSet::new(),
            TIME,
            &f.families,
        )
        .unwrap();
    let (scope, tuple) = f.tuple(1, "A");
    assert_eq!(
        verified
            .partition(f.node.public_key(), scope, &tuple, TIME)
            .unwrap()[0]
            .rights,
        follower.rights
    );
    let mut custody = f.partitions[0].clone();
    custody.rights = [Right::Custody].into();
    custody.fields = FieldSelection::Only(BTreeSet::new());
    let inherited = f.families.partitions(&[custody]);
    assert_eq!(inherited.len(), 2);
    assert!(
        inherited
            .iter()
            .all(|grant| grant.rights == [Right::Custody].into()
                && grant.fields == FieldSelection::Only(BTreeSet::new()))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegation_checks_resolved_authority_and_cannot_remove_follower_restrictions() {
    let f = Fixture::new();
    let mut read_only = f.resources[1].clone();
    read_only.rights = [Right::Read].into();
    let chain = f.chain(vec![f.resources[0].clone(), read_only], vec![]);
    let delegable = chain.delegable();
    let identity = PrivateKey::from_bytes(&[6; 32]);
    let node = PrivateKey::from_bytes(&[7; 32]);
    let terms = (1, Some(AuthorRange::new(100, 200).unwrap()));
    assert!(
        delegable
            .delegate_with_families(
                &f.identity,
                identity.public_key(),
                PrincipalKind::User,
                vec![f.resources[0].clone()],
                vec![],
                terms,
                (TIME, 1000, 0),
                &f.families
            )
            .is_err()
    );
    // A correctly signed chain crafted with the schema-free API must fail receipt too.
    let mut forged = delegable.clone();
    let mut capability = forged.0.last().unwrap().capability.clone();
    capability.subject = identity.public_key();
    capability.delegations = 1;
    capability.grants = vec![f.resources[0].clone()];
    let mut extended = capability.extended().unwrap();
    extended.authors = terms.1;
    capability.extension = crate::Extension::new(extended).unwrap();
    forged
        .0
        .push(crate::SignedCapability::sign(capability, &f.identity).unwrap());
    let forged = forged.certify(&identity, node.public_key(), 100).unwrap();
    assert!(
        forged
            .verified_with_families(
                f.root.public_key(),
                &RevocationSet::new(),
                TIME,
                &f.families
            )
            .is_err()
    );
    let mut narrow = f.resources[1].clone();
    narrow.rights = [Right::Read].into();
    let delegated = delegable
        .delegate_with_families(
            &f.identity,
            identity.public_key(),
            PrincipalKind::User,
            vec![narrow],
            vec![],
            terms,
            (TIME, 1000, 0),
            &f.families,
        )
        .unwrap()
        .certify(&identity, node.public_key(), 100)
        .unwrap();
    let verified = delegated
        .verified_with_families(
            f.root.public_key(),
            &RevocationSet::new(),
            TIME,
            &f.families,
        )
        .unwrap();
    let grants = verified
        .author(node.public_key(), 100, f.resources[1].scope.table, TIME)
        .unwrap();
    assert_eq!(grants[0].rights, [Right::Read].into());
    assert!(
        verified
            .author(node.public_key(), 100, f.resources[0].scope.table, TIME)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn duplicate_follower_grants_cannot_hide_a_restriction_during_compaction() {
    let f = Fixture::new();
    let mut parent_read = f.resources[0].clone();
    parent_read.rights = [Right::Read].into();
    let mut child_read = f.resources[1].clone();
    child_read.rights = [Right::Read].into();
    let grants = vec![
        parent_read,
        f.resources[0].clone(),
        child_read.clone(),
        child_read,
    ];
    assert_eq!(f.families.compact_resources(&grants), grants);
    let mut parent_read = f.partitions[0].clone();
    parent_read.rights = [Right::Read].into();
    let mut child_read = f.partitions[1].clone();
    child_read.rights = [Right::Read].into();
    let grants = vec![
        parent_read,
        f.partitions[0].clone(),
        child_read.clone(),
        child_read,
    ];
    assert_eq!(f.families.compact_partitions(&grants), grants);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn wire_grant_quota_stays_bounded_when_profiles_can_resolve_wide_schemas() {
    let f = Fixture::new();
    let authority = Authority::owner_in(
        &f.root,
        crate::DatabaseContext {
            publisher: f.root.public_key(),
            scope: f.resources[0].scope.domain,
        },
    )
    .unwrap();
    assert!(
        authority
            .issue_partition_terms(
                f.identity.public_key(),
                vec![f.resources[0].clone(); crate::MAX_GRANTS + 1],
                vec![],
                (1, Some(AuthorRange::new(1, 1000).unwrap())),
                (TIME, 3600, 0)
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn old_sparse_grants_never_acquire_family_authority() {
    let f = Fixture::new();
    let chain = f.old_chain(vec![f.resources[0].clone()], vec![f.partitions[0].clone()]);
    assert!(!chain.held().unwrap().extended().unwrap().families);
    let verified = chain
        .verified_with_families(
            f.root.public_key(),
            &RevocationSet::new(),
            TIME,
            &f.families,
        )
        .unwrap();
    let (scope, tuple) = f.tuple(1, "A");
    assert!(
        verified
            .partition(f.node.public_key(), scope, &tuple, TIME)
            .is_err()
    );
    let bindings: Vec<_> = chain
        .0
        .iter()
        .map(|link| link.author_binding.clone())
        .collect();
    assert!(
        crate::AuthorBinding::verify_chain_with_families(
            &bindings,
            f.root.public_key(),
            f.resources[1].scope.table,
            1,
            f.node.public_key(),
            &f.families
        )
        .is_err()
    );
    let marked = f.chain(vec![f.resources[0].clone()], vec![f.partitions[0].clone()]);
    assert!(
        marked
            .verified(f.root.public_key(), &RevocationSet::new(), TIME)
            .is_err()
    );
    let bindings: Vec<_> = marked
        .0
        .iter()
        .map(|link| link.author_binding.clone())
        .collect();
    crate::AuthorBinding::verify_chain_with_families(
        &bindings,
        f.root.public_key(),
        f.resources[1].scope.table,
        1,
        f.node.public_key(),
        &f.families,
    )
    .unwrap();
    let mut tampered = marked.clone();
    let mut extended = tampered.0[0].capability.extended().unwrap();
    extended.families = false;
    tampered.0[0].capability.extension = crate::Extension::new(extended).unwrap();
    assert!(
        tampered
            .verified_with_families(
                f.root.public_key(),
                &RevocationSet::new(),
                TIME,
                &f.families
            )
            .is_err()
    );
}
