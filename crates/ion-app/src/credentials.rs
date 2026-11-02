//! Host credential lookup. Provider adapters request keys at dispatch time;
//! neither Session history nor terminal rendering owns them.
use ion_ai::BoxFuture;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub trait CredentialResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        stop: CancellationToken,
    ) -> BoxFuture<'a, Result<Option<String>, CredentialResolutionError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialResolutionError {
    #[error("credential refresh failed")]
    RefreshFailed,
    #[error("credential store is unavailable")]
    Unavailable,
    #[error("credential resolution was cancelled")]
    Cancelled,
}

impl<F> CredentialResolver for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    fn resolve<'a>(
        &'a self,
        stop: CancellationToken,
    ) -> BoxFuture<'a, Result<Option<String>, CredentialResolutionError>> {
        Box::pin(async move {
            if stop.is_cancelled() {
                Err(CredentialResolutionError::Cancelled)
            } else {
                Ok(self())
            }
        })
    }
}
