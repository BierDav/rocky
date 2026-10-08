use crate::*;

impl Envelope {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let mut bytes = Vec::new();
        for id in [
            self.scope.mesh.0,
            self.scope.app.0,
            self.scope.domain.0,
            self.caller.0,
        ] {
            bytes.extend(id);
        }
        for version in [
            self.versions.protocol,
            self.versions.schema,
            self.versions.operation,
        ] {
            bytes.extend(version.to_le_bytes());
        }
        put_id(&mut bytes, self.id);
        bytes.extend(self.lamport.get().to_le_bytes());
        put_id(&mut bytes, self.entity);
        bytes.extend((self.dependencies.len() as u32).to_le_bytes());
        for id in &self.dependencies {
            put_id(&mut bytes, *id);
        }
        bytes.extend(
            u32::try_from(self.payload.len())
                .map_err(|_| ProtocolError::InvalidLength)?
                .to_le_bytes(),
        );
        bytes.extend(&self.payload);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let mut reader = Reader(bytes);
        let scope = Scope {
            mesh: MeshId(reader.array()?),
            app: AppId(reader.array()?),
            domain: DomainId(reader.array()?),
        };
        let caller = PrincipalId(reader.array()?);
        let versions = Versions {
            protocol: reader.u32()?,
            schema: reader.u32()?,
            operation: reader.u32()?,
        };
        versions.validate()?;
        let id = reader.id()?;
        let lamport = LamportTime::new(reader.u64()?)?;
        let entity = reader.id()?;
        let count = reader.u32()?;
        if count > 2 {
            return Err(ProtocolError::InvalidDependencies);
        }
        let mut dependencies = Vec::with_capacity(count as usize);
        for _ in 0..count {
            dependencies.push(reader.id()?);
        }
        let length = reader.u32()? as usize;
        if length > MAX_PAYLOAD_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        if reader.0.len() < length {
            return Err(ProtocolError::Truncated);
        }
        if reader.0.len() > length {
            return Err(ProtocolError::TrailingBytes);
        }
        let envelope = Self {
            scope,
            caller,
            versions,
            id,
            lamport,
            entity,
            dependencies,
            payload: reader.0.to_vec(),
        };
        envelope.validate()?;
        Ok(envelope)
    }
}
fn put_id(bytes: &mut Vec<u8>, id: OperationId) {
    bytes.extend(id.node.0);
    bytes.extend(id.sequence.get().to_le_bytes());
}
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let (head, tail) = self.0.split_at_checked(N).ok_or(ProtocolError::Truncated)?;
        let mut result = [0; N];
        result.copy_from_slice(head);
        self.0 = tail;
        Ok(result)
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn id(&mut self) -> Result<OperationId, ProtocolError> {
        Ok(OperationId {
            node: NodeId(self.array()?),
            sequence: AuthorSequence::new(self.u64()?)?,
        })
    }
}
