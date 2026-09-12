use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;

use crate::{ModelRequest, ModelStreamEvent, ProviderError};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;

pub trait ModelService: Send + Sync {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>>;
}
