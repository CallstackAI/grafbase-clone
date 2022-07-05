use crate::ServerError;
use dynaql_parser::Pos;
use std::fmt::{Debug, Display};
use thiserror::Error;

/// The purpose of this structure is to prepare for ID Obfuscation withing Dynaql
pub struct ObfuscatedID<'a> {
    ty: &'a str,
    id: &'a str,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ObfuscatedIDError {
    #[error("You are trying to manipulate an entity with the wrong query.")]
    InvalidType { expected: String, current: String },
    #[error("Batch requests are not supported")]
    InvalidID,
}
impl ObfuscatedIDError {
    pub fn into_server_error(self, pos: Pos) -> ServerError {
        crate::Error::new_with_source(self).into_server_error(pos)
    }
}

impl<'a> ObfuscatedID<'a> {
    pub fn new(id: &'a str) -> Result<Self, ObfuscatedIDError> {
        match id.rsplit_once('#') {
            Some((ty, id)) => Ok(Self { ty, id }),
            _ => Err(ObfuscatedIDError::InvalidID),
        }
    }

    pub fn expect(id: &'a str, ty: &'a str) -> Result<Self, ObfuscatedIDError> {
        let id = Self::new(id)?;

        if id.ty == ty {
            Ok(id)
        } else {
            Err(ObfuscatedIDError::InvalidType {
                expected: ty.to_string(),
                current: id.ty.to_string(),
            })
        }
    }

    pub fn ty(&'a self) -> &'a str {
        self.ty
    }

    pub fn id(&'a self) -> &'a str {
        self.id
    }
}

impl<'a> Display for ObfuscatedID<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.ty, self.id)
    }
}
