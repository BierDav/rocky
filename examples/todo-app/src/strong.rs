use crate::{Error, MAX_TITLE_BYTES, VisibleRow};
use rocky_core::{AuthorSequence, NodeId, OperationId, ProtocolError};

pub fn encode_snapshot(rows: &[VisibleRow]) -> Result<Vec<u8>, Error> {
    if rows.windows(2).any(|p| p[0].id >= p[1].id) {
        return Err(Error::InvalidProjection);
    }
    let mut b = Vec::new();
    b.extend(
        u32::try_from(rows.len())
            .map_err(|_| ProtocolError::InvalidLength)?
            .to_le_bytes(),
    );
    for row in rows {
        if row.title.len() > MAX_TITLE_BYTES || row.title.trim().is_empty() {
            return Err(Error::InvalidTitle);
        }
        b.extend(row.id.node.0);
        b.extend(row.id.sequence.get().to_le_bytes());
        b.extend((row.title.len() as u32).to_le_bytes());
        b.extend(row.title.as_bytes());
        b.push(u8::from(row.completed));
    }
    Ok(b)
}
pub fn decode_snapshot(mut b: &[u8]) -> Result<Vec<VisibleRow>, Error> {
    fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8], Error> {
        let (h, t) = b.split_at_checked(n).ok_or(ProtocolError::Truncated)?;
        *b = t;
        Ok(h)
    }
    fn array<const N: usize>(b: &mut &[u8]) -> Result<[u8; N], Error> {
        take(b, N)?.try_into().map_err(|_| Error::InvalidProjection)
    }
    let count = u32::from_le_bytes(array(&mut b)?) as usize;
    if count > b.len() / 29 {
        return Err(ProtocolError::InvalidLength.into());
    }
    let mut rows = Vec::new();
    for _ in 0..count {
        let id = OperationId {
            node: NodeId(array(&mut b)?),
            sequence: AuthorSequence::new(u64::from_le_bytes(array(&mut b)?))?,
        };
        let n = u32::from_le_bytes(array(&mut b)?) as usize;
        if n > MAX_TITLE_BYTES {
            return Err(Error::InvalidTitle);
        }
        let title = std::str::from_utf8(take(&mut b, n)?)
            .map_err(|_| Error::InvalidUtf8)?
            .to_owned();
        let completed = match take(&mut b, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(Error::InvalidBoolean),
        };
        rows.push(VisibleRow {
            id,
            title,
            completed,
        });
    }
    if !b.is_empty() {
        return Err(ProtocolError::TrailingBytes.into());
    }
    encode_snapshot(&rows)?;
    Ok(rows)
}
