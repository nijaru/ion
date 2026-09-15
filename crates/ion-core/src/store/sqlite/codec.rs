//! Row encoding and decoding.
//!
//! Stored JSON is decoded strictly. A record this build cannot read is an error
//! that names the row, never a silently absent value: "unknown" and "unsupported
//! evidence" are different facts, and only the first may be treated as not yet
//! done.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{StoreError, id_from};
use crate::id::IdError;

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::Failed(format!("cannot encode durable record: {error}")))
}

pub(crate) fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    serde_json::from_str(raw)
        .map_err(|error| StoreError::Failed(format!("cannot decode durable record: {error}")))
}

pub(crate) fn decode_optional<T: DeserializeOwned>(
    raw: Option<String>,
) -> Result<Option<T>, StoreError> {
    raw.map(|raw| decode(&raw)).transpose()
}

pub(crate) fn id_optional<T: TryFrom<i64, Error = IdError>>(
    raw: Option<i64>,
) -> Result<Option<T>, StoreError> {
    raw.map(id_from).transpose()
}

/// A stored byte count or size that must fit SQLite's signed integers.
pub(crate) fn as_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::Failed(format!("value {value} does not fit a sqlite integer")))
}
