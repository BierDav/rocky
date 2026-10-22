use crate::{ApplicationManifest, Commitment, ProtocolError};

/// Field references in one exact source table, in canonical name order.
pub struct FieldOrdinals<'a> {
    fields: Vec<&'a crate::DefinitionItem>,
    ordinals: std::collections::BTreeMap<Commitment, u32>,
}

impl ApplicationManifest {
    pub fn field_ordinals(&self, table: Commitment) -> Result<FieldOrdinals<'_>, ProtocolError> {
        let table = self
            .entities
            .iter()
            .find(|entity| entity.id == table)
            .ok_or(ProtocolError::InvalidManifest)?;
        let prefix = format!("{}.", table.name);
        let mut fields: Vec<_> = self
            .fields
            .iter()
            .filter(|field| !field.name.contains('.') || field.name.starts_with(&prefix))
            .collect();
        fields.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let ordinals = fields
            .iter()
            .enumerate()
            .map(|(ordinal, field)| {
                Ok((
                    field.id,
                    u32::try_from(ordinal).map_err(|_| ProtocolError::InvalidManifest)?,
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, ProtocolError>>()?;
        if ordinals.len() != fields.len() {
            return Err(ProtocolError::InvalidManifest);
        }
        Ok(FieldOrdinals { fields, ordinals })
    }
}

impl FieldOrdinals<'_> {
    pub fn ordinal(&self, field: Commitment) -> Option<u32> {
        self.ordinals.get(&field).copied()
    }

    pub fn field(&self, ordinal: u32) -> Option<Commitment> {
        self.fields.get(ordinal as usize).map(|field| field.id)
    }

    pub fn name(&self, ordinal: u32) -> Option<&str> {
        self.fields
            .get(ordinal as usize)
            .map(|field| field.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthorLogScope, DefinitionItem, DomainId, DomainRule};

    #[test]
    fn source_table_names_determine_ordinals_and_reject_foreign_fields() {
        let publisher = [7; 32];
        let mut manifest = ApplicationManifest {
            publisher,
            name: "ordinal-test".into(),
            app: ApplicationManifest::app_id(publisher, "ordinal-test"),
            schema: 1,
            projection: 1,
            entities: Vec::new(),
            fields: Vec::new(),
            operations: Vec::new(),
            domains: vec![DomainRule {
                domain: DomainId([8; 16]),
                author_log: AuthorLogScope::Domain,
                rule: "test".into(),
            }],
        };
        let item = |kind, name: &str| DefinitionItem {
            id: manifest.item_id(kind, name),
            name: name.into(),
            shape: "test".into(),
        };
        let entities = vec![item("entity", "rows"), item("entity", "other")];
        let fields = vec![
            item("field", "rows.z"),
            item("field", "other.a"),
            item("field", "rows.a"),
        ];
        manifest.entities = entities;
        manifest.fields = fields;
        manifest.validate().unwrap();
        let ordinals = manifest.field_ordinals(manifest.entities[0].id).unwrap();
        assert_eq!(ordinals.field(0), Some(manifest.fields[2].id));
        assert_eq!(ordinals.field(1), Some(manifest.fields[0].id));
        assert_eq!(ordinals.field(2), None);
        assert_eq!(ordinals.ordinal(manifest.fields[1].id), None);
        assert_eq!(ordinals.name(0), Some("rows.a"));
        assert!(manifest.field_ordinals([0; 32]).is_err());
        let original = manifest.fields[0].id;
        manifest.fields.remove(2);
        let ordinals = manifest.field_ordinals(manifest.entities[0].id).unwrap();
        assert_eq!(ordinals.ordinal(original), Some(0));
        assert_eq!(ordinals.field(1), None);
    }
}
