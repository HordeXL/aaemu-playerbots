//! Server → client parsers (see [`crate::sc`]).

use crate::stream::MarshalError;

#[derive(Debug, Clone)]
pub enum WorldEvent {}

pub fn parse(_level: u8, _opcode: u16, _body: &[u8]) -> Result<Option<WorldEvent>, MarshalError> {
    Ok(None)
}
