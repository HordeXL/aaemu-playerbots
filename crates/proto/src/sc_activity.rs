//! Server → client parsers (see [`crate::sc`]).

use crate::stream::MarshalError;

#[derive(Debug, Clone)]
pub enum ActivityEvent {}

pub fn parse(
    _level: u8,
    _opcode: u16,
    _body: &[u8],
) -> Result<Option<ActivityEvent>, MarshalError> {
    Ok(None)
}
