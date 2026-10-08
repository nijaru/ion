use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;

use crate::{
    GenerationControls, ModelRef, ModelRequest, ModelStreamEvent, ProviderError, ProviderErrorKind,
    Reasoning,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;

pub trait ModelService: Send + Sync {
    /// Pure route preflight: no credentials, transport or generation. Implementations
    /// accepting explicit reasoning must validate it with their wire compiler.
    /// Acceptance describes adapter support, not remote entitlement or reliability.
    fn validate_controls(
        &self,
        _model: &ModelRef,
        controls: &GenerationControls,
    ) -> Result<(), ProviderError> {
        controls.validate()?;
        if controls.reasoning != Reasoning::ProviderDefault {
            return Err(ProviderError {
                kind: ProviderErrorKind::Unsupported,
                message: "explicit reasoning controls are unsupported by this model service".into(),
                retry_after_ms: None,
            });
        }
        Ok(())
    }

    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>>;
}
