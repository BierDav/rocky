use crate::{AppId, Commitment, DomainId, Envelope, OperationId, Scope, Versions, commitment};

/// Maximum encoded definition size, independent of operation payload limits.
pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum items in each definition group, including fields of wide applications.
pub const MAX_MANIFEST_ITEMS: usize = 32_768;
/// Maximum declared domains in one manifest.
pub const MAX_MANIFEST_DOMAINS: usize = 4096;
/// The one manifest body format; other bodies are refused.
const MANIFEST_FORMAT: &[u8] = b"rocky.manifest.v2";

/// Unsigned definition data. Installing this body never installs executable code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationManifest {
    pub publisher: [u8; 32],
    pub name: String,
    pub app: AppId,
    pub schema: u32,
    pub projection: u32,
    pub entities: Vec<DefinitionItem>,
    pub fields: Vec<DefinitionItem>,
    pub operations: Vec<DefinitionItem>,
    pub domains: Vec<DomainRule>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionItem {
    pub id: Commitment,
    pub name: String,
    /// Language-independent canonical shape/rule description.
    pub shape: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainRule {
    pub domain: DomainId,
    pub author_log: AuthorLogScope,
    pub rule: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorLogScope {
    Domain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopedOperationId {
    pub scope: Scope,
    pub operation: OperationId,
}
impl Envelope {
    pub fn scoped_id(&self) -> ScopedOperationId {
        ScopedOperationId {
            scope: self.scope,
            operation: self.id,
        }
    }
    pub fn scoped_dependencies(&self) -> Vec<ScopedOperationId> {
        self.dependencies
            .iter()
            .map(|operation| ScopedOperationId {
                scope: self.scope,
                operation: *operation,
            })
            .collect()
    }
}
impl ApplicationManifest {
    pub fn app_id(publisher: [u8; 32], name: &str) -> AppId {
        let mut bytes = publisher.to_vec();
        put(&mut bytes, name);
        let hash = commitment("rocky.manifest.app.v1", &bytes);
        let mut id = [0; 16];
        id.copy_from_slice(&hash[..16]);
        AppId(id)
    }
    pub fn item_id(&self, kind: &str, name: &str) -> Commitment {
        definition_item_id(self.publisher, self.app, kind, name)
    }
    pub fn validate(&self) -> Result<(), crate::ProtocolError> {
        if self.app != Self::app_id(self.publisher, &self.name)
            || self.name.is_empty()
            || self.schema == 0
            || self.projection == 0
            || self.domains.is_empty()
            || self.domains.len() > MAX_MANIFEST_DOMAINS
        {
            return Err(crate::ProtocolError::InvalidManifest);
        }
        let mut ids = std::collections::BTreeSet::new();
        for (kind, items) in [
            ("entity", &self.entities),
            ("field", &self.fields),
            ("operation", &self.operations),
        ] {
            if items.len() > MAX_MANIFEST_ITEMS {
                return Err(crate::ProtocolError::InvalidManifest);
            }
            for item in items {
                if item.name.is_empty()
                    || item.shape.is_empty()
                    || item.id != self.item_id(kind, &item.name)
                    || !ids.insert(item.id)
                {
                    return Err(crate::ProtocolError::InvalidManifest);
                }
            }
        }
        let mut domains = std::collections::BTreeSet::new();
        if self
            .domains
            .iter()
            .any(|domain| domain.rule.is_empty() || !domains.insert(domain.domain))
        {
            return Err(crate::ProtocolError::InvalidManifest);
        }
        if self.canonical_body().len() > MAX_MANIFEST_BYTES {
            return Err(crate::ProtocolError::InvalidManifest);
        }
        Ok(())
    }
    /// Little-endian counts/versions and length-prefixed UTF-8; IDs sorted by bytes.
    pub fn canonical_body(&self) -> Vec<u8> {
        let mut bytes = MANIFEST_FORMAT.to_vec();
        bytes.extend(self.publisher);
        put(&mut bytes, &self.name);
        bytes.extend(self.app.0);
        for version in [self.schema, self.projection] {
            bytes.extend(version.to_le_bytes());
        }
        for items in [&self.entities, &self.fields, &self.operations] {
            let mut items: Vec<_> = items.iter().collect();
            items.sort_by_key(|item| item.id);
            bytes.extend((items.len() as u32).to_le_bytes());
            for item in items {
                bytes.extend(item.id);
                put(&mut bytes, &item.name);
                put(&mut bytes, &item.shape);
            }
        }
        let mut domains: Vec<_> = self.domains.iter().collect();
        domains.sort_by_key(|domain| domain.domain);
        bytes.extend((domains.len() as u32).to_le_bytes());
        for domain in domains {
            bytes.extend(domain.domain.0);
            bytes.push(0);
            put(&mut bytes, &domain.rule);
        }
        bytes
    }
    /// Decode canonical definition data without resolving or loading handlers.
    pub fn from_body(bytes: &[u8]) -> Result<Self, crate::ProtocolError> {
        let invalid = || crate::ProtocolError::InvalidManifest;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(invalid());
        }
        let mut body = Body(bytes);
        if body.take(MANIFEST_FORMAT.len())? != MANIFEST_FORMAT {
            return Err(invalid());
        }
        let publisher = body.array()?;
        let name = body.text()?;
        let app = AppId(body.array()?);
        let schema = body.number()?;
        let projection = body.number()?;
        let mut groups = Vec::new();
        for _ in 0..3 {
            let count = body.number()?;
            if count as usize > MAX_MANIFEST_ITEMS {
                return Err(invalid());
            }
            let mut items = Vec::new();
            for _ in 0..count {
                items.push(DefinitionItem {
                    id: body.array()?,
                    name: body.text()?,
                    shape: body.text()?,
                });
            }
            groups.push(items);
        }
        let count = body.number()?;
        if count as usize > MAX_MANIFEST_DOMAINS {
            return Err(invalid());
        }
        let mut domains = Vec::new();
        for _ in 0..count {
            let domain = DomainId(body.array()?);
            if body.take(1)? != [0] {
                return Err(invalid());
            }
            domains.push(DomainRule {
                domain,
                author_log: AuthorLogScope::Domain,
                rule: body.text()?,
            });
        }
        let mut groups = groups.into_iter();
        let manifest = Self {
            publisher,
            name,
            app,
            schema,
            projection,
            entities: groups.next().ok_or_else(invalid)?,
            fields: groups.next().ok_or_else(invalid)?,
            operations: groups.next().ok_or_else(invalid)?,
            domains,
        };
        manifest.validate()?;
        if !body.0.is_empty() || manifest.canonical_body() != bytes {
            return Err(invalid());
        }
        Ok(manifest)
    }
    pub fn hash(&self) -> Commitment {
        commitment("rocky.manifest.body.v1", &self.canonical_body())
    }
    pub fn supports(&self, scope: Scope, versions: Versions, projection: u32) -> bool {
        self.app == scope.app
            && self.schema == versions.schema
            && self.projection == projection
            && self.domains.iter().any(|rule| rule.domain == scope.domain)
    }
}
/// Identity of a named definition item under its publisher and application.
pub fn definition_item_id(publisher: [u8; 32], app: AppId, kind: &str, name: &str) -> Commitment {
    let mut bytes = publisher.to_vec();
    bytes.extend(app.0);
    put(&mut bytes, kind);
    put(&mut bytes, name);
    commitment("rocky.manifest.item.v1", &bytes)
}
fn put(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend((text.len() as u32).to_le_bytes());
    bytes.extend(text.as_bytes());
}

struct Body<'a>(&'a [u8]);
impl<'a> Body<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], crate::ProtocolError> {
        let value = self
            .0
            .get(..length)
            .ok_or(crate::ProtocolError::InvalidManifest)?;
        self.0 = &self.0[length..];
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], crate::ProtocolError> {
        self.take(N)?
            .try_into()
            .map_err(|_| crate::ProtocolError::InvalidManifest)
    }
    fn number(&mut self) -> Result<u32, crate::ProtocolError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn text(&mut self) -> Result<String, crate::ProtocolError> {
        let length = self.number()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| crate::ProtocolError::InvalidManifest)
    }
}
