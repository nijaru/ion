use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;

use super::{TaskKind, TaskKindName};

#[derive(Default)]
pub struct TaskRegistry {
    kinds: HashMap<(TaskKindName, u32), Arc<dyn TaskKind>>,
}

impl TaskRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        kind: TaskKindName,
        schema_version: u32,
        implementation: Arc<dyn TaskKind>,
    ) -> Result<(), TaskRegistryError> {
        let key = (kind, schema_version);
        if self.kinds.contains_key(&key) {
            return Err(TaskRegistryError::Duplicate {
                kind: key.0,
                schema_version,
            });
        }
        self.kinds.insert(key, implementation);
        Ok(())
    }

    pub(crate) fn get(
        &self,
        kind: &TaskKindName,
        schema_version: u32,
    ) -> Option<Arc<dyn TaskKind>> {
        self.kinds
            .get(&(kind.clone(), schema_version))
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskRegistryError {
    #[error("task kind {kind} schema {schema_version} is already registered")]
    Duplicate {
        kind: TaskKindName,
        schema_version: u32,
    },
}
