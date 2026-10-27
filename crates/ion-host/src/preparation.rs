//! Retained stop-and-join custody for Host preparation outside Core's slot.
use anyhow::Result;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct Retained<T> {
    task: Option<JoinHandle<Result<T>>>,
}
pub(crate) struct Preparation<T> {
    stop: CancellationToken,
    retained: Arc<Mutex<Retained<T>>>,
}
impl<T: Send + 'static> Preparation<T> {
    pub(crate) fn start<F, W>(stop: CancellationToken, work: F) -> Self
    where
        F: FnOnce(CancellationToken) -> W + Send + 'static,
        W: Future<Output = Result<T>> + Send + 'static,
    {
        let retained = Arc::new(Mutex::new(Retained { task: None }));
        let mut state = retained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let custody = retained.clone();
        let token = stop.clone();
        // The worker retains its custodian through completion even if the control
        // is dropped. Unlike an aborted preparation future, this always joins
        // its blocking work. Explicit joining also retrieves its result.
        state.task = Some(tokio::spawn(async move {
            let _custody = custody;
            work(token).await
        }));
        drop(state);
        Self { stop, retained }
    }
    pub(crate) fn request_stop(&self) {
        self.stop.cancel();
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }
    pub(crate) async fn join(&mut self) -> Result<T> {
        poll_fn(|cx| {
            let mut retained = self
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let task = retained
                .task
                .as_mut()
                .expect("preparation completion consumed once");
            match Pin::new(task).poll(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(result) => {
                    retained.task = None;
                    std::task::Poll::Ready(result.unwrap_or_else(|error| {
                        let context = format!("preparation task failed: {error}");
                        Err(anyhow::Error::new(error).context(context))
                    }))
                }
            }
        })
        .await
    }
}
impl<T> Drop for Preparation<T> {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
