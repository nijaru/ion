use serde::{Deserialize, Serialize};

use crate::ArtifactId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub uri: String,
    pub stored_bytes: u64,
    pub total_bytes: u64,
    pub sha256: [u8; 32],
    pub truncated: bool,
}
