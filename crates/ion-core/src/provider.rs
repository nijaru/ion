//! Provider execution boundary for the Turn runtime.
//!
//! This is deliberately provider-specific rather than a generic Effect backend. The
//! boundary receives a frozen provider-neutral semantic request, advertises the exact
//! semantic adapter/encoding compatibility it implements, and owns provider start
//! evidence/reconciliation.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ion_ai::{BoxFuture, ModelStream, ProviderError, Usage};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    AttemptId, ContentDigest, ProviderBinding, ProviderBindingId, ProviderStartReceipt,
    SemanticCompatibilityId, SemanticRequest, StartReceiptCapability,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelBoundaryIdentity {
    pub binding: ProviderBindingId,
    pub adapter: SemanticCompatibilityId,
    pub request_encoding: SemanticCompatibilityId,
}

pub enum ModelStart {
    Started {
        stream: ModelStream,
        start_receipt: Option<ProviderStartReceipt>,
    },
    NotStarted {
        reason: String,
    },
    Indeterminate {
        reason: String,
        usage: Usage,
        start_receipt: Option<ProviderStartReceipt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StartReconciliation {
    NotStarted { reason: String },
    Started(ProviderStartReceipt),
    Unknown { reason: String },
}

pub trait ModelBoundary: Send + Sync {
    fn identity(&self) -> ModelBoundaryIdentity;

    /// Hash the exact provider-specific canonical request representation produced by
    /// this frozen encoding revision. The stable logical effect key is supplied because
    /// adapters that send it as idempotency material must cover it in the fingerprint.
    /// Authentication, trace IDs and transport-local metadata must not influence this digest.
    fn fingerprint(
        &self,
        request: &SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError>;

    /// Cross the provider start boundary for one already-durable AttemptId.
    fn start<'a>(
        &'a self,
        attempt: AttemptId,
        effect_key: String,
        request: SemanticRequest,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart>;

    fn start_receipts(&self) -> StartReceiptCapability {
        StartReceiptCapability::None
    }

    /// Called only from explicit recovery/resume. A boundary that advertises
    /// authoritative receipts must return NotStarted only when its durable receipt
    /// store proves the AttemptId never crossed the start boundary.
    fn reconcile_start<'a>(
        &'a self,
        _attempt: AttemptId,
        _effect_key: String,
    ) -> BoxFuture<'a, StartReconciliation> {
        Box::pin(async {
            StartReconciliation::Unknown {
                reason: "provider has no authoritative durable start receipt".to_owned(),
            }
        })
    }
}

#[derive(Clone, Default)]
pub struct ModelBoundaries {
    inner: Arc<BTreeMap<ProviderBindingId, Arc<dyn ModelBoundary>>>,
}

impl fmt::Debug for ModelBoundaries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelBoundaries")
            .field("bindings", &self.inner.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ModelBoundaries {
    pub fn new(
        boundaries: impl IntoIterator<Item = Arc<dyn ModelBoundary>>,
    ) -> Result<Self, ModelBoundaryError> {
        let mut inner = BTreeMap::new();
        for boundary in boundaries {
            let identity = boundary.identity();
            if inner.insert(identity.binding.clone(), boundary).is_some() {
                return Err(ModelBoundaryError::Duplicate(
                    identity.binding.as_str().to_owned(),
                ));
            }
        }
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    #[must_use]
    pub fn get(&self, id: &ProviderBindingId) -> Option<Arc<dyn ModelBoundary>> {
        self.inner.get(id).cloned()
    }

    pub fn resolve(
        &self,
        binding: &ProviderBinding,
    ) -> Result<Arc<dyn ModelBoundary>, ModelBoundaryError> {
        let boundary = self
            .get(&binding.id)
            .ok_or_else(|| ModelBoundaryError::Missing(binding.id.as_str().to_owned()))?;
        let identity = boundary.identity();
        if identity.adapter != binding.adapter {
            return Err(ModelBoundaryError::Incompatible {
                binding: binding.id.as_str().to_owned(),
                fact: "adapter",
            });
        }
        if identity.request_encoding != binding.request_encoding {
            return Err(ModelBoundaryError::Incompatible {
                binding: binding.id.as_str().to_owned(),
                fact: "request encoding",
            });
        }
        if boundary.start_receipts() != binding.start_receipts {
            return Err(ModelBoundaryError::Incompatible {
                binding: binding.id.as_str().to_owned(),
                fact: "start-receipt contract",
            });
        }
        Ok(boundary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelBoundaryError {
    #[error("provider boundary {0:?} is duplicated")]
    Duplicate(String),
    #[error("provider boundary {0:?} is unavailable")]
    Missing(String),
    #[error("provider boundary {binding:?} has incompatible {fact}")]
    Incompatible { binding: String, fact: &'static str },
}
