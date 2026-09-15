use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable bounded task result/scratch/progress state.
///
/// A large opaque artifact reference is deliberately absent: no boundary yet owns
/// publication, lookup or integrity checks, so a field that could only ever be
/// `None` would imply spilling support that does not exist. Reintroduce it with
/// the boundary that implements `DESIGN.md` §17's publish-before-reference rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutput {
    pub value: Value,
}
