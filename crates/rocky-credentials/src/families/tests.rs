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
            &json!({"format":6,"publisher":publisher,"application":"families","tables":tables})
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
        let authority = Authority::owner_in(
            &self.root,
            crate::DatabaseContext {
                publisher: self.root.public_key(),
                scope: self.resources[0].scope.domain,
            },
        )
        .unwrap();
        let capability = authority
            .issue_partition_terms(
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
    for chain in [f.chain(resources, partitions), f.chain(compact, selectors)] {
        let bytes = crate::encode(&chain).unwrap();
        let ids: Vec<_> = chain.0.iter().map(|link| link.id().unwrap()).collect();
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
    let forged = delegable
        .delegate(
            &f.identity,
            identity.public_key(),
            PrincipalKind::User,
            vec![f.resources[0].clone()],
            terms.0,
            terms.1,
            (TIME, 1000, 0),
        )
        .unwrap()
        .certify(&identity, node.public_key(), 100)
        .unwrap();
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
