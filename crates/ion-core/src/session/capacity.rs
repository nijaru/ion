use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::ResourceDomain;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn domains_have_independent_capacity() {
        let capacity = TaskCapacity::default()
            .with_limit(ResourceDomain::Model, NonZeroUsize::new(1).unwrap())
            .with_limit(ResourceDomain::Tool, NonZeroUsize::new(1).unwrap());
        let model = capacity.acquire(Some(ResourceDomain::Model)).await;
        assert!(
            capacity.limits[&ResourceDomain::Model]
                .clone()
                .try_acquire_owned()
                .is_err()
        );
        let tool = capacity.acquire(Some(ResourceDomain::Tool)).await;
        assert!(tool.is_some());
        drop(model);
        assert!(
            capacity.limits[&ResourceDomain::Model]
                .clone()
                .try_acquire_owned()
                .is_ok()
        );
    }
}

/// Unconfigured domains are unlimited. Limits apply independently, not globally.
#[derive(Debug, Clone, Default)]
pub struct TaskCapacity {
    limits: HashMap<ResourceDomain, Arc<Semaphore>>,
}

impl TaskCapacity {
    #[must_use]
    pub fn with_limit(mut self, domain: ResourceDomain, limit: NonZeroUsize) -> Self {
        self.limits
            .insert(domain, Arc::new(Semaphore::new(limit.get())));
        self
    }

    pub(super) async fn acquire(
        &self,
        domain: Option<ResourceDomain>,
    ) -> Option<OwnedSemaphorePermit> {
        let semaphore = self.limits.get(&domain?)?;
        Some(
            semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("capacity is never closed"),
        )
    }
}
