//! Immutable out-of-line content references.

use serde::{Deserialize, Serialize};

use crate::ContentDigest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub digest: ContentDigest,
    pub length: u64,
    pub media_type: Option<String>,
    pub encoding: Option<String>,
}

impl BlobRef {
    #[must_use]
    pub const fn new(digest: ContentDigest, length: u64) -> Self {
        Self {
            digest,
            length,
            media_type: None,
            encoding: None,
        }
    }
}
