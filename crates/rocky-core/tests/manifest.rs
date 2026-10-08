use rocky_core::*;
fn manifest() -> ApplicationManifest {
    let mut manifest = ApplicationManifest {
        publisher: [1; 32],
        name: "example".into(),
        app: ApplicationManifest::app_id([1; 32], "example"),
        schema: 1,
        projection: 2,
        handler: 3,
        entities: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        operations: Vec::new(),
        domains: vec![DomainRule {
            domain: DomainId([2; 16]),
            author_log: AuthorLogScope::Domain,
            rule: "independent".into(),
        }],
    };
    for name in ["first", "second"] {
        manifest.entities.push(DefinitionItem {
            id: manifest.item_id("entity", name),
            name: name.into(),
            shape: "record".into(),
        });
    }
    manifest
}
#[test]
fn canonical_definitions_are_bounded_and_publisher_bound() {
    let manifest = manifest();
    let bytes = manifest.canonical_body();
    let decoded = ApplicationManifest::from_body(&bytes).unwrap();
    assert_eq!(decoded.hash(), manifest.hash());
    let mut reordered = manifest.clone();
    reordered.entities.reverse();
    assert_eq!(reordered.canonical_body(), bytes);
    for length in 0..bytes.len() {
        assert!(ApplicationManifest::from_body(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(ApplicationManifest::from_body(&trailing).is_err());
    let mut forged = manifest.clone();
    forged.publisher = [3; 32];
    assert!(forged.validate().is_err());
    let mut duplicate = manifest.clone();
    duplicate.entities.push(duplicate.entities[0].clone());
    assert!(duplicate.validate().is_err());
    let mut upgraded = manifest;
    upgraded.handler = 4;
    assert_ne!(upgraded.hash(), decoded.hash());
}

#[test]
fn wide_definitions_round_trip_with_fields_and_domains_without_changing_encoding() {
    let mut wide = manifest();
    wide.entities.clear();
    for index in 0..2000 {
        let name = format!("table-{index}");
        wide.entities.push(DefinitionItem {
            id: wide.item_id("entity", &name),
            name: name.clone(),
            shape: "record".into(),
        });
        for column in 0..6 {
            let name = format!("{name}.field-{column}");
            wide.fields.push(DefinitionItem {
                id: wide.item_id("field", &name),
                name,
                shape: "integer".into(),
            });
        }
    }
    wide.domains = (0u16..256)
        .map(|index| {
            let mut bytes = [0; 16];
            bytes[..2].copy_from_slice(&index.to_le_bytes());
            DomainRule {
                domain: DomainId(bytes),
                author_log: AuthorLogScope::Domain,
                rule: "independent".into(),
            }
        })
        .collect();
    wide.validate().unwrap();
    let bytes = wide.canonical_body();
    assert!(bytes.len() > MAX_PAYLOAD_BYTES && bytes.len() < MAX_MANIFEST_BYTES);
    assert_eq!(
        ApplicationManifest::from_body(&bytes)
            .unwrap()
            .canonical_body(),
        bytes
    );
    let hash = wide.hash();
    wide.entities.reverse();
    wide.fields.reverse();
    wide.domains.reverse();
    assert_eq!(wide.hash(), hash);
    let offset = b"rocky.manifest.v1".len() + 32 + 4 + wide.name.len() + 16 + 12;
    let mut oversized = bytes.clone();
    oversized[offset..offset + 4].copy_from_slice(&((MAX_MANIFEST_ITEMS + 1) as u32).to_le_bytes());
    assert!(ApplicationManifest::from_body(&oversized).is_err());
    assert!(ApplicationManifest::from_body(&vec![0; MAX_MANIFEST_BYTES + 1]).is_err());
    wide.fields.push(wide.fields[0].clone());
    assert!(wide.validate().is_err());
}
