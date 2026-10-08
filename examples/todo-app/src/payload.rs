use crate::{COMPLETED_FIELD_ID, Error, TITLE_FIELD_ID};
use rocky_core::ProtocolError;

pub const MAX_TITLE_BYTES: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionKind {
    Create = 0,
    Title = 1,
    Completed = 2,
    Delete = 3,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    CreateTodo { title: String, completed: bool },
    SetTitle(String),
    SetCompleted(bool),
    DeleteTodo,
}
impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::CreateTodo { .. } => ActionKind::Create,
            Self::SetTitle(_) => ActionKind::Title,
            Self::SetCompleted(_) => ActionKind::Completed,
            Self::DeleteTodo => ActionKind::Delete,
        }
    }
    pub fn fields(&self) -> &'static [u32] {
        match self {
            Self::CreateTodo { .. } => &[TITLE_FIELD_ID, COMPLETED_FIELD_ID],
            Self::SetTitle(_) => &[TITLE_FIELD_ID],
            Self::SetCompleted(_) => &[COMPLETED_FIELD_ID],
            Self::DeleteTodo => &[],
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![self.kind() as u8];
        match self {
            Self::CreateTodo { title, completed } => {
                put_title(&mut bytes, title)?;
                bytes.push(u8::from(*completed));
            }
            Self::SetTitle(title) => put_title(&mut bytes, title)?,
            Self::SetCompleted(value) => bytes.push(u8::from(*value)),
            Self::DeleteTodo => {}
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let (&tag, mut rest) = bytes.split_first().ok_or(ProtocolError::Truncated)?;
        let action = match tag {
            0 => Self::CreateTodo {
                title: take_title(&mut rest)?,
                completed: take_bool(&mut rest)?,
            },
            1 => Self::SetTitle(take_title(&mut rest)?),
            2 => Self::SetCompleted(take_bool(&mut rest)?),
            3 => Self::DeleteTodo,
            other => return Err(Error::UnknownAction(other)),
        };
        if !rest.is_empty() {
            return Err(ProtocolError::TrailingBytes.into());
        }
        Ok(action)
    }
}
fn validate_title(title: &str) -> Result<(), Error> {
    if title.len() > MAX_TITLE_BYTES || !title.chars().any(|c| !c.is_whitespace()) {
        return Err(Error::InvalidTitle);
    }
    Ok(())
}
fn put_title(bytes: &mut Vec<u8>, title: &str) -> Result<(), Error> {
    validate_title(title)?;
    bytes.extend((title.len() as u32).to_le_bytes());
    bytes.extend(title.as_bytes());
    Ok(())
}
fn take_title(bytes: &mut &[u8]) -> Result<String, Error> {
    let (length, rest) = bytes.split_at_checked(4).ok_or(ProtocolError::Truncated)?;
    let length =
        u32::from_le_bytes(length.try_into().map_err(|_| ProtocolError::Truncated)?) as usize;
    if length > MAX_TITLE_BYTES {
        return Err(Error::InvalidTitle);
    }
    let (title, rest) = rest
        .split_at_checked(length)
        .ok_or(ProtocolError::Truncated)?;
    let title = std::str::from_utf8(title).map_err(|_| Error::InvalidUtf8)?;
    validate_title(title)?;
    *bytes = rest;
    Ok(title.to_owned())
}
fn take_bool(bytes: &mut &[u8]) -> Result<bool, Error> {
    let (&value, rest) = bytes.split_first().ok_or(ProtocolError::Truncated)?;
    let value = match value {
        0 => false,
        1 => true,
        _ => return Err(Error::InvalidBoolean),
    };
    *bytes = rest;
    Ok(value)
}
