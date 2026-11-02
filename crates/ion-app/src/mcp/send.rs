//! The SDK's request handle acknowledges queueing, not completion of its send.
//! Carry one local completion signal in the request, without a parallel registry.
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use rmcp::{
    RoleClient,
    model::{GetExtensions, JsonRpcMessage},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use tokio::sync::oneshot;

#[derive(Clone)]
pub(super) struct Completion(Arc<Mutex<Option<oneshot::Sender<()>>>>);

impl Completion {
    pub fn attach(request: &mut impl GetExtensions) -> oneshot::Receiver<()> {
        let (done, joined) = oneshot::channel();
        request
            .extensions_mut()
            .insert(Self(Arc::new(Mutex::new(Some(done)))));
        joined
    }
}

pub(super) struct Tracked<T>(pub T);

impl<T: Transport<RoleClient>> Transport<RoleClient> for Tracked<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        mut item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let done = match &mut item {
            JsonRpcMessage::Request(request) => request
                .request
                .extensions_mut()
                .remove::<Completion>()
                .and_then(|completion| {
                    completion
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                }),
            _ => None,
        };
        let work = self.0.send(item);
        async move {
            let result = work.await;
            if let Some(done) = done {
                let _ = done.send(());
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.0.receive().await
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.0.close().await
    }
}
